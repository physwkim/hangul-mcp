//! `Cargo.lock` 에서 rhwp 의 버전과 소스를 읽어 컴파일 타임 환경변수로 넘긴다.
//!
//! rhwp 는 git rev 로 고정된 의존성이라 크레이트 버전(0.7.x)만으로는 어떤 커밋으로
//! 빌드됐는지 알 수 없다. 실행 중인 서버가 어느 rev 인지 `server_info` 도구가 답할 수
//! 있게 여기서 박아 넣는다.

use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    let lock = fs::read_to_string("Cargo.lock").expect("Cargo.lock 읽기 실패");
    let (version, source) = rhwp_entry(&lock).expect("Cargo.lock 에 rhwp 패키지가 없다");
    println!("cargo:rustc-env=RHWP_VERSION={version}");
    println!("cargo:rustc-env=RHWP_SOURCE={source}");
}

/// `[[package]]` 블록 중 `name = "rhwp"` 인 것의 `version` 과 `source` 를 뽑는다.
///
/// 경로 의존성(`path = "../rhwp"`)이면 `source` 줄이 없으므로 `"path"` 를 반환한다.
fn rhwp_entry(lock: &str) -> Option<(String, String)> {
    let mut in_rhwp = false;
    let mut version = None;

    for line in lock.lines() {
        let line = line.trim();

        if line == "[[package]]" {
            if in_rhwp {
                // source 줄 없이 rhwp 블록이 끝났다 — 경로 의존성.
                break;
            }
            version = None;
            continue;
        }
        if let Some(name) = line.strip_prefix("name = ") {
            in_rhwp = name.trim_matches('"') == "rhwp";
            continue;
        }
        if !in_rhwp {
            continue;
        }
        if let Some(v) = line.strip_prefix("version = ") {
            version = Some(v.trim_matches('"').to_string());
        }
        if let Some(s) = line.strip_prefix("source = ") {
            return Some((version?, s.trim_matches('"').to_string()));
        }
    }

    version.map(|v| (v, "path".to_string()))
}
