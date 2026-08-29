//! 검증 인프라 (M1) — "완료의 정의를 데이터로".
//!
//! 설계 근거: docs/agent-upgrade/04_DESIGN.md §6.2.
//! 핵심 불변식: VERIFYING → DONE 전이는 오직 시스템이 verification 명령을 직접
//! 실행해 전부 통과하고 검증자가 pass 를 낸 경우에만 일어난다. 모델 출력은
//! 이 모듈의 어떤 생성 경로(`CmdResults`, `VerifierVerdict`)도 건드릴 수 없다 —
//! 두 타입의 생성자가 이 모듈 내부에만 존재하기 때문이다(가시성으로 봉인).

pub mod guard;
pub mod plan;
pub mod runner;
pub mod spec;
pub mod task;

pub use plan::PlanDoc;
pub use spec::SpecDoc;
pub use runner::{ratchet_check, run_task_verification};
pub use task::{TaskDoc, TaskState};
