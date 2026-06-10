//! 열린 문서 세션 저장소.
//!
//! `open_document`가 발급한 `doc_id`로 메모리 내 `DocumentCore`를 찾는다.
//! 모든 접근은 `with_session`/`with_session_mut`를 통해 Mutex 아래에서 수행된다.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rhwp::document_core::DocumentCore;
use rmcp::ErrorData;

/// 열린 문서 하나의 세션.
pub struct DocumentSession {
    pub core: DocumentCore,
    /// 원본 파일 경로 (저장 시 기본 대상).
    pub path: PathBuf,
    /// 마지막 저장 이후 편집이 있었는지.
    pub dirty: bool,
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
            dirty: false,
        };
        self.sessions
            .lock()
            .expect("document store poisoned")
            .insert(id.clone(), session);
        id
    }

    /// 세션을 제거하고 (path, dirty)를 반환한다.
    pub fn remove(&self, doc_id: &str) -> Result<(PathBuf, bool), ErrorData> {
        self.sessions
            .lock()
            .expect("document store poisoned")
            .remove(doc_id)
            .map(|s| (s.path, s.dirty))
            .ok_or_else(|| unknown_doc(doc_id))
    }

    /// 열린 문서 목록: (doc_id, path, dirty).
    pub fn list(&self) -> Vec<(String, PathBuf, bool)> {
        self.sessions
            .lock()
            .expect("document store poisoned")
            .iter()
            .map(|(id, s)| (id.clone(), s.path.clone(), s.dirty))
            .collect()
    }

    /// 읽기 전용 접근.
    pub fn with_session<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&DocumentSession) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let guard = self.sessions.lock().expect("document store poisoned");
        let session = guard.get(doc_id).ok_or_else(|| unknown_doc(doc_id))?;
        f(session)
    }

    /// 변경 접근. 클로저가 Ok를 반환하면 dirty를 세운다.
    pub fn with_session_edit<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&mut DocumentSession) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let mut guard = self.sessions.lock().expect("document store poisoned");
        let session = guard.get_mut(doc_id).ok_or_else(|| unknown_doc(doc_id))?;
        let result = f(session)?;
        session.dirty = true;
        Ok(result)
    }

    /// 변경 접근이되 dirty를 건드리지 않는다 (저장 등).
    pub fn with_session_mut<T>(
        &self,
        doc_id: &str,
        f: impl FnOnce(&mut DocumentSession) -> Result<T, ErrorData>,
    ) -> Result<T, ErrorData> {
        let mut guard = self.sessions.lock().expect("document store poisoned");
        let session = guard.get_mut(doc_id).ok_or_else(|| unknown_doc(doc_id))?;
        f(session)
    }
}

fn unknown_doc(doc_id: &str) -> ErrorData {
    ErrorData::invalid_params(
        format!("알 수 없는 doc_id: {doc_id} (open_document로 먼저 여세요)"),
        None,
    )
}
