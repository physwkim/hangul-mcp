//! 열린 문서 세션 저장소.
//!
//! `open_document`가 발급한 `doc_id`로 메모리 내 `DocumentCore`를 찾는다.
//! 모든 접근은 `with_session`(읽기)/`with_session_edit`(편집)/`with_session_save`(저장)와
//! `undo`/`redo` 를 통해 Mutex 아래에서 수행된다.
//!
//! 편집 불변식: 문서를 바꾸는 도구는 모두 `with_session_edit` 를 거친다. 그 안에서
//! 성공한 호출은 되돌리기 한 단위가 되고, 실패한 호출은 문서를 호출 전 그대로 남긴다
//! (여러 단계 편집이 중간에 실패해도 반쯤 바뀐 문서가 남지 않는다).
//!
//! 기록 불변식: 문서는 언제나 `History::state` 가 가리키는 상태다. 문서를 통째로 바꾸는
//! 스냅샷 뜨기·복원은 `DocumentSession::snapshot`/`restore` 만 하고, 기록은 교체가 성공한
//! 뒤에만 옮긴다. 실패나 panic 뒤 이 불변식을 다시 세우지 못하면 세션을 `fault` 로 막아,
//! 닫기 전까지 어떤 도구도 상태를 모르는 문서를 읽거나 저장하지 못한다.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use rhwp::document_core::DocumentCore;
use rmcp::ErrorData;

/// 되돌리기 기록 상한. 스냅샷은 문서 전체 복제라 그림이 많은 문서에서는 메모리를 바로 먹는다.
/// rhwp 스냅샷 저장소 상한(`DocumentCore::MAX_SNAPSHOTS`)보다 충분히 작아야 rhwp 가
/// 우리 스냅샷을 몰래 축출하지 않는다.
pub const MAX_UNDO: usize = 20;

/// 열린 문서 하나의 세션.
pub struct DocumentSession {
    pub core: DocumentCore,
    /// 원본 파일 경로 (저장 시 기본 대상).
    pub path: PathBuf,
    history: History,
    /// 문서가 어느 상태인지 알 수 없게 된 이유. `Some` 이면 닫기 외의 접근을 거부한다.
    fault: Option<String>,
}

/// 편집 기록. 문서 상태마다 다시 쓰지 않는 번호를 매기고, 저장한 상태 번호와 지금 상태
/// 번호가 같은지로 미저장 여부를 정한다 — undo/redo 로 저장 시점에 돌아오면 깨끗해지고,
/// 되돌린 뒤 새로 편집하면 같은 깊이여도 다른 상태가 된다.
struct History {
    /// (그 상태의 rhwp 스냅샷 id, 상태 번호). 끝이 가장 최근.
    undo: Vec<(u32, u64)>,
    redo: Vec<(u32, u64)>,
    state: u64,
    saved_state: u64,
    next_state: u64,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            state: 0,
            saved_state: 0,
            next_state: 1,
        }
    }
}

impl DocumentSession {
    /// 마지막 저장(또는 열기) 이후 상태가 바뀌었는지.
    pub fn is_dirty(&self) -> bool {
        self.history.state != self.history.saved_state
    }

    /// 되돌리기 기록 상태.
    pub fn history_status(&self) -> HistoryStatus {
        HistoryStatus {
            undo_available: self.history.undo.len(),
            redo_available: self.history.redo.len(),
            dirty: self.is_dirty(),
        }
    }

    fn step(&mut self, undo: bool) -> Result<(), ErrorData> {
        let from = if undo {
            &self.history.undo
        } else {
            &self.history.redo
        };
        let Some(&(snap, state)) = from.last() else {
            let what = if undo { "되돌릴" } else { "다시 할" };
            return Err(ErrorData::invalid_params(
                format!("{what} 편집이 없다"),
                None,
            ));
        };
        let current = self.snapshot()?;
        if let Err(e) = self.restore(snap) {
            let back = self.restore(current);
            self.core.discard_snapshot_native(current);
            return Err(match back {
                Ok(()) => ErrorData::internal_error(format!("스냅샷 {snap} 복원 실패: {e}"), None),
                Err(be) => self.fault(format!(
                    "스냅샷 {snap} 복원 실패: {e} (원래 상태로도 돌아가지 못함: {be})"
                )),
            });
        }
        self.core.discard_snapshot_native(snap);
        let h = &mut self.history;
        let (from, to) = if undo {
            (&mut h.undo, &mut h.redo)
        } else {
            (&mut h.redo, &mut h.undo)
        };
        from.pop();
        to.push((current, h.state));
        h.state = state;
        Ok(())
    }

    /// 지금 문서의 스냅샷을 뜬다. rhwp 는 뜨기 전에 셀 배치값을 문서에 반영하므로,
    /// 그 도중 panic 하면 문서가 어느 상태인지 모른다.
    fn snapshot(&mut self) -> Result<u32, ErrorData> {
        match catch_unwind(AssertUnwindSafe(|| self.core.save_snapshot_native())) {
            Ok(id) => Ok(id),
            Err(payload) => Err(self.fault(format!("스냅샷 실패: {}", panic_message(payload)))),
        }
    }

    /// 문서를 스냅샷 상태로 갈아끼운다. Err 면 문서가 반쯤 바뀌었을 수 있다 — rhwp 는
    /// 문서를 바꾼 뒤 파생 상태를 다시 만들므로 그 도중 panic 할 수 있다.
    fn restore(&mut self, snap: u32) -> Result<(), String> {
        match catch_unwind(AssertUnwindSafe(|| self.core.restore_snapshot_native(snap))) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(payload) => Err(format!("panic: {}", panic_message(payload))),
        }
    }

    /// 기록 불변식을 다시 세우지 못했다. 세션을 막고 그 사실을 오류로 돌려준다.
    fn fault(&mut self, reason: String) -> ErrorData {
        let err = fault_error(&reason);
        self.fault = Some(reason);
        err
    }

    /// 성공한 편집 하나를 기록한다. `before` 는 편집 직전 스냅샷.
    fn record_edit(&mut self, before: u32) {
        let h = &mut self.history;
        h.undo.push((before, h.state));
        h.state = h.next_state;
        h.next_state += 1;
        for (snap, _) in h.redo.drain(..) {
            self.core.discard_snapshot_native(snap);
        }
        while h.undo.len() > MAX_UNDO {
            let (snap, _) = h.undo.remove(0);
            self.core.discard_snapshot_native(snap);
        }
    }
}

#[derive(Default)]
pub struct DocumentStore {
    sessions: Mutex<HashMap<String, DocumentSession>>,
    next_id: AtomicU64,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 세션을 등록하고 새 doc_id를 반환한다.
    pub fn insert(&self, core: DocumentCore, path: PathBuf) -> String {
        let id = format!("doc-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let session = DocumentSession {
            core,
            path,
            history: History::default(),
            fault: None,
        };
        self.lock().insert(id.clone(), session);
        id
    }

    /// 세션을 제거하고 (path, dirty)를 반환한다.
    pub fn remove(&self, doc_id: &str) -> Result<(PathBuf, bool), ErrorData> {
        self.lock()
            .remove(doc_id)
            .map(|s| {
                let dirty = s.is_dirty();
                (s.path, dirty)
            })
            .ok_or_else(|| unknown_doc(doc_id))
    }

    /// 열린 문서 목록: (doc_id, path, dirty, 막힌 이유).
    pub fn list(&self) -> Vec<(String, PathBuf, bool, Option<String>)> {
        self.lock()
            .iter()
            .map(|(id, s)| (id.clone(), s.path.clone(), s.is_dirty(), s.fault.clone()))
            .collect()
    }

    /// 읽기 전용 접근.
    pub fn with_session<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&DocumentSession) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let mut guard = self.lock();
        let session = live(&mut guard, doc_id)?;
        no_panic(|| f(session))
    }

    /// 편집 접근 — 문서를 바꾸는 유일한 관문. 호출 전 스냅샷을 떠서, 클로저가 Ok 면
    /// 되돌리기 기록에 넣고, Err 면 그 스냅샷으로 문서를 되돌린 뒤 오류를 돌려준다.
    pub fn with_session_edit<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&mut DocumentSession) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let mut guard = self.lock();
        let session = live(&mut guard, doc_id)?;
        let before = session.snapshot()?;
        match no_panic(|| f(&mut *session)) {
            Ok(result) => {
                session.record_edit(before);
                Ok(result)
            }
            Err(e) => {
                let restored = session.restore(before);
                session.core.discard_snapshot_native(before);
                match restored {
                    Ok(()) => Err(e),
                    Err(re) => {
                        Err(session
                            .fault(format!("{} (실패한 편집을 되돌리지 못함: {re})", e.message)))
                    }
                }
            }
        }
    }

    /// 저장 접근. 문서는 읽기만 하고, 클로저가 Ok 면 지금 상태를 저장된 상태로 기록한다.
    pub fn with_session_save<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&DocumentSession) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let mut guard = self.lock();
        let session = live(&mut guard, doc_id)?;
        let result = no_panic(|| f(session))?;
        session.history.saved_state = session.history.state;
        Ok(result)
    }

    /// 마지막 편집을 되돌린다. 반환: 그 뒤의 기록 상태.
    pub fn undo(&self, doc_id: &str) -> Result<HistoryStatus, ErrorData> {
        self.step(doc_id, true)
    }

    /// 되돌린 편집을 다시 한다. 반환: 그 뒤의 기록 상태.
    pub fn redo(&self, doc_id: &str) -> Result<HistoryStatus, ErrorData> {
        self.step(doc_id, false)
    }

    /// 저장소 잠금. 클로저 panic 은 `no_panic` 이 잠금 안에서 잡으므로 poison 은 잠금을 쥔 채
    /// 저장소 코드 자체가 panic 한 경우뿐이다. 맵은 insert/remove 단위로만 바뀌어 그때도
    /// 일관되므로, poison 으로 이후 모든 호출을 죽이지 않고 그대로 이어 쓴다.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, DocumentSession>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn step(&self, doc_id: &str, undo: bool) -> Result<HistoryStatus, ErrorData> {
        let mut guard = self.lock();
        let session = live(&mut guard, doc_id)?;
        session.step(undo)?;
        Ok(session.history_status())
    }
}

/// 되돌리기 기록 상태.
pub struct HistoryStatus {
    pub undo_available: usize,
    pub redo_available: usize,
    pub dirty: bool,
}

/// 클로저 panic(rhwp 내부 인덱스 오류 등)을 도구 오류로 바꾼다. 그대로 두면 응답 없이
/// 요청 task 가 죽고 저장소 잠금이 poison 된다. 편집 중 panic 은 `with_session_edit` 가
/// 다른 Err 와 똑같이 스냅샷으로 되돌린다.
fn no_panic<T>(f: impl FnOnce() -> Result<T, ErrorData>) -> Result<T, ErrorData> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| {
        Err(ErrorData::internal_error(
            format!("rhwp 내부 오류(panic): {}", panic_message(payload)),
            None,
        ))
    })
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "알 수 없는 panic".into())
}

/// 막히지 않은 세션. 막힌 세션은 닫고 다시 여는 것 말고는 쓸 수 없다.
fn live<'a>(
    sessions: &'a mut HashMap<String, DocumentSession>,
    doc_id: &str,
) -> Result<&'a mut DocumentSession, ErrorData> {
    let session = sessions
        .get_mut(doc_id)
        .ok_or_else(|| unknown_doc(doc_id))?;
    match &session.fault {
        Some(reason) => Err(fault_error(reason)),
        None => Ok(session),
    }
}

fn fault_error(reason: &str) -> ErrorData {
    ErrorData::internal_error(
        format!(
            "{reason} — 문서가 어느 상태인지 알 수 없어 이 세션을 막았다. 저장하지 않은 변경은 \
             살릴 수 없으니 close_document 로 닫고 open_document 로 다시 여세요"
        ),
        None,
    )
}

fn unknown_doc(doc_id: &str) -> ErrorData {
    ErrorData::invalid_params(
        format!("알 수 없는 doc_id: {doc_id} (open_document로 먼저 여세요)"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_doc() -> (DocumentStore, String) {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ref_text.hwpx");
        let core = DocumentCore::from_bytes(&std::fs::read(path).unwrap()).unwrap();
        let store = DocumentStore::new();
        let id = store.insert(core, PathBuf::from(path));
        (store, id)
    }

    fn para0(store: &DocumentStore, id: &str) -> String {
        store
            .with_session(id, |s| {
                Ok(s.core.document().sections[0].paragraphs[0].text.clone())
            })
            .unwrap()
    }

    /// 편집 도중 실패하면 이미 바꾼 부분까지 되돌아가고, 되돌리기 기록도 미저장 표시도 남지 않는다.
    #[test]
    fn failed_edit_rolls_back_partial_mutation() {
        let (store, id) = store_with_doc();
        let before = para0(&store, &id);
        let err = store.with_session_edit(&id, |s| {
            s.core.insert_text_native(0, 0, 0, "반쯤").unwrap();
            Err::<(), _>(ErrorData::invalid_params("둘째 단계 실패", None))
        });
        assert!(err.is_err());
        assert_eq!(para0(&store, &id), before, "실패한 편집의 앞 단계가 남음");
        let status = store.with_session(&id, |s| Ok(s.history_status())).unwrap();
        assert_eq!((status.undo_available, status.dirty), (0, false));
    }

    /// 편집 클로저가 panic 해도 오류로 돌아오고, 앞 단계는 되돌려지며, 저장소는 계속 쓸 수 있다.
    #[test]
    fn panicking_edit_rolls_back_and_store_stays_usable() {
        let (store, id) = store_with_doc();
        let before = para0(&store, &id);
        let err = store
            .with_session_edit(&id, |s| -> Result<(), ErrorData> {
                s.core.insert_text_native(0, 0, 0, "반쯤").unwrap();
                panic!("둘째 단계 panic");
            })
            .unwrap_err();
        assert!(err.message.contains("둘째 단계 panic"), "{}", err.message);
        assert_eq!(para0(&store, &id), before, "panic 한 편집의 앞 단계가 남음");
        let status = store.with_session(&id, |s| Ok(s.history_status())).unwrap();
        assert_eq!((status.undo_available, status.dirty), (0, false));
        assert!(store
            .with_session(&id, |_| -> Result<(), ErrorData> { panic!("읽기 panic") })
            .is_err());
        assert_eq!(store.list().len(), 1);
    }

    fn fault_of(store: &DocumentStore, id: &str) -> Option<String> {
        store.list().into_iter().find(|d| d.0 == id).unwrap().3
    }

    /// 실패한 편집을 되돌리지 못하면 세션이 막혀, 닫기 전까지 읽기·편집·undo·저장을 모두 거부한다.
    #[test]
    fn unrecoverable_edit_rollback_faults_session() {
        let (store, id) = store_with_doc();
        let err = store
            .with_session_edit(&id, |s| -> Result<(), ErrorData> {
                // 스냅샷 저장소가 빈 새 core 로 갈아끼워 편집 전 스냅샷 복원이 실패하게 한다.
                s.core = DocumentCore::new_empty();
                Err(ErrorData::invalid_params("편집 실패", None))
            })
            .unwrap_err();
        assert!(err.message.contains("되돌리지 못함"), "{}", err.message);
        assert!(fault_of(&store, &id).is_some());
        let blocked = [
            store.with_session(&id, |_| Ok(())).unwrap_err(),
            store.with_session_edit(&id, |_| Ok(())).unwrap_err(),
            store.with_session_save(&id, |_| Ok(())).unwrap_err(),
            store.undo(&id).err().unwrap(),
            store.redo(&id).err().unwrap(),
        ];
        for e in blocked {
            assert!(e.message.contains("close_document"), "{}", e.message);
        }
        store.remove(&id).unwrap();
        assert!(store.list().is_empty());
    }

    /// undo 대상 스냅샷을 복원하지 못해도 지금 상태로 돌아오면 세션은 막히지 않고,
    /// 문서·기록·미저장 표시가 호출 전 그대로다.
    #[test]
    fn failed_undo_restore_leaves_document_and_history_unchanged() {
        let (store, id) = store_with_doc();
        store
            .with_session_edit(&id, |s| {
                s.core.insert_text_native(0, 0, 0, "편집").unwrap();
                Ok(())
            })
            .unwrap();
        let edited = para0(&store, &id);
        {
            let mut guard = store.lock();
            let s = guard.get_mut(&id).unwrap();
            let snap = s.history.undo[0].0;
            s.core.discard_snapshot_native(snap);
        }
        let err = store.undo(&id).err().unwrap();
        assert!(err.message.contains("복원 실패"), "{}", err.message);
        assert_eq!(fault_of(&store, &id), None);
        assert_eq!(para0(&store, &id), edited);
        let status = store.with_session(&id, |s| Ok(s.history_status())).unwrap();
        assert_eq!(
            (status.undo_available, status.redo_available, status.dirty),
            (1, 0, true)
        );
    }

    /// 잠금을 쥔 채 panic 해 poison 된 저장소도 이후 호출을 그대로 받는다.
    #[test]
    fn poisoned_lock_is_recovered() {
        let (store, id) = store_with_doc();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = store.sessions.lock().unwrap();
            panic!("잠금 쥔 채 panic");
        }));
        assert!(store.sessions.is_poisoned());
        assert_eq!(para0(&store, &id), para0(&store, &id));
        assert_eq!(store.list().len(), 1);
    }
}
