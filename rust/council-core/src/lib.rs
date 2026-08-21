pub mod adapter;
pub mod engine;
pub mod metrics;
pub mod model;
pub mod prompts;
pub mod providers;

pub use adapter::{AgentAdapter, CouncilError, CouncilResult};
pub use engine::{Council, CycleOutcome, StopReason};
pub use metrics::{Metrics, MetricsReport, NaturalnessRating};
pub use model::{AgentId, AgentState, Author, Decision, Intent, Room, RoomEvent, RoomSnapshot};
