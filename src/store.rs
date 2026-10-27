//! 열린 문서 세션 저장소.
//!
//! `open_document`가 발급한 `doc_id`로 메모리 내 `DocumentCore`를 찾는다.
//! 모든 접근은 `with_session`(읽기)/`with_session_edit`(편집)/`with_session_save`(저장)와
//! `undo`/`redo` 를 통해 Mutex 아래에서 수행된다.
//!
//! 편집 불변식: 문서를 바꾸는 도구는 모두 `with_session_edit` 를 거친다. 그 안에서
//! 성공한 호출은 되돌리기 한 단위가 되고, 실패한 호출은 문서를 호출 전 그대로 남긴다
//! (여러 단계 편집이 중간에 실패해도 반쯤 바뀐 문서가 남지 않는다).

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
        let h = &mut self.history;
        let (from, to) = if undo {
            (&mut h.undo, &mut h.redo)
        } else {
            (&mut h.redo, &mut h.undo)
        };
        let Some((snap, state)) = from.pop() else {
            let what = if undo { "되돌릴" } else { "다시 할" };
            return Err(ErrorData::invalid_params(
                format!("{what} 편집이 없다"),
                None,
            ));
        };
        let current = self.core.save_snapshot_native();
        if let Err(e) = self.core.restore_snapshot_native(snap) {
            self.core.discard_snapshot_native(current);
            from.push((snap, state));
            return Err(ErrorData::internal_error(
                format!("스냅샷 {snap} 복원 실패: {e}"),
                None,
            ));
        }
        self.core.discard_snapshot_native(snap);
        to.push((current, h.state));
        h.state = state;
        Ok(())
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

    /// 열린 문서 목록: (doc_id, path, dirty).
    pub fn list(&self) -> Vec<(String, PathBuf, bool)> {
        self.lock()
            .iter()
            .map(|(id, s)| (id.clone(), s.path.clone(), s.is_dirty()))
            .collect()
    }

    /// 읽기 전용 접근.
    pub fn with_session<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&DocumentSession) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let guard = self.lock();
        let session = guard.get(doc_id).ok_or_else(|| unknown_doc(doc_id))?;
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
        let session = guard.get_mut(doc_id).ok_or_else(|| unknown_doc(doc_id))?;
        let before = session.core.save_snapshot_native();
        match no_panic(|| f(&mut *session)) {
            Ok(result) => {
                session.record_edit(before);
                Ok(result)
            }
            Err(e) => {
                let restored = session.core.restore_snapshot_native(before);
                session.core.discard_snapshot_native(before);
                match restored {
                    Ok(_) => Err(e),
                    Err(re) => Err(ErrorData::internal_error(
                        format!("{} (실패한 편집을 되돌리지 못함: {re})", e.message),
                        None,
                    )),
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
        let session = guard.get_mut(doc_id).ok_or_else(|| unknown_doc(doc_id))?;
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
        let session = guard.get_mut(doc_id).ok_or_else(|| unknown_doc(doc_id))?;
        no_panic(|| session.step(undo))?;
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
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "알 수 없는 panic".into());
        Err(ErrorData::internal_error(
            format!("rhwp 내부 오류(panic): {msg}"),
            None,
        ))
    })
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
