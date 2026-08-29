//! 테스트 무결성 가드 (M1) — 태스크 diff 에서 테스트 약화를 자동 감지한다.
//! 근거: docs/agent-upgrade/04_DESIGN.md §6.6. 레드팀 시나리오 3·4의 차단 장치.

/// diff 텍스트(통합 diff)에서 테스트 약화 징후를 찾는다. 위반 사유 목록 반환.
/// 빈 목록 = 통과. diff 가 비어 있으면(변경 없음) 검사 대상이 없다.
pub fn check_test_integrity(diff_text: &str) -> Vec<String> {
    let mut violations = Vec::new();
    if diff_text.trim().is_empty() {
        return violations;
    }
    let mut current_file = String::new();
    let mut in_tests = false;
    let mut added_ignores: Vec<String> = Vec::new();
    let mut removed_tests: Vec<String> = Vec::new();
    let mut added_asserts = 0usize;
    let mut removed_asserts = 0usize;

    for line in diff_text.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = path.to_string();
            in_tests = is_test_path(&current_file);
            continue;
        }
        if let Some(path) = line.strip_prefix("--- a/") {
            let _ = path;
            continue;
        }
        if line.starts_with("@@") || line.starts_with("diff ") || line.starts_with("index ") {
            continue;
        }
        if !in_tests {
            continue;
        }
        let Some(body) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) else {
            continue;
        };
        let added = line.starts_with('+');
        let trimmed = body.trim();
        if added && trimmed.starts_with("#[ignore") {
            added_ignores.push(current_file.clone());
        }
        if !added && trimmed.starts_with("#[test]") {
            removed_tests.push(current_file.clone());
        }
        if trimmed.contains("assert") {
            if added {
                added_asserts += 1;
            } else {
                removed_asserts += 1;
            }
        }
    }

    for f in &added_ignores {
        violations.push(format!(
            "테스트 무결성: {f} 에 #[ignore] 가 추가됐다 — 검증자 승인 없이는 금지"
        ));
    }
    for f in &removed_tests {
        violations.push(format!(
            "테스트 무결성: {f} 에서 #[test] 함수가 삭제됐다 — 테스트 수 래칫 위반"
        ));
    }
    if removed_asserts > added_asserts {
        violations.push(format!(
            "테스트 무결성: 어서션 순감소(-{removed_asserts}/+{added_asserts}) — 기대값 약화 의심"
        ));
    }
    violations
}

/// tests/acceptance/ 는 SPEC 동결 산출물 — Executor 의 어떤 변경도 승인 사유 없이는 금지.
pub fn check_acceptance_immutable(diff_text: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let mut flagged = std::collections::HashSet::new();
    for line in diff_text.lines() {
        let Some(path) = line.strip_prefix("+++ b/") else {
            continue;
        };
        if path.contains("tests/acceptance/") && flagged.insert(path.to_string()) {
            violations.push(format!(
                "인수 테스트 불변: {path} 는 SPEC 동결 산출물 — 검증자 승인 없이 수정 금지"
            ));
        }
    }
    violations
}

fn is_test_path(path: &str) -> bool {
    path.contains("tests/")
        || path.contains("_test")
        || path.contains("_tests")
        || path.contains("test_")
        || path.ends_with("test.rs")
        || path.ends_with("_test.go")
        || path.ends_with("_test.py")
        || path.contains("/tests/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF_IGNORE_ADDED: &str = "\
--- a/tests/foo.rs
+++ b/tests/foo.rs
@@ -1,4 +1,5 @@
 #[test]
+#[ignore]
 fn broken_case() {
-    assert_eq!(2 + 2, 5);
+    assert_eq!(2 + 2, 4);
 }
";

    const DIFF_TEST_DELETED: &str = "\
--- a/tests/foo.rs
+++ b/tests/foo.rs
@@ -1,5 +1 @@
-#[test]
-fn gone() {
-    assert_eq!(1, 1);
-}
 fn kept() {}
";

    #[test]
    fn ignore_addition_is_caught() {
        let v = check_test_integrity(DIFF_IGNORE_ADDED);
        assert!(
            v.iter().any(|s| s.contains("#[ignore]")),
            "ignore 추가 감지: {v:?}"
        );
    }

    #[test]
    fn test_deletion_is_caught() {
        let v = check_test_integrity(DIFF_TEST_DELETED);
        assert!(
            v.iter().any(|s| s.contains("#[test] 함수가 삭제")),
            "테스트 삭제 감지: {v:?}"
        );
    }

    #[test]
    fn clean_diff_passes() {
        let diff = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
 fn a() {}
+fn b() {}
";
        assert!(check_test_integrity(diff).is_empty());
        assert!(check_test_integrity("").is_empty(), "변경 없음은 위반 아님(별도 require_diff 로 차단)");
    }
}
