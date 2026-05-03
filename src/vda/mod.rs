//! VDA 5050-shaped transport structs and adapter helpers.
//!
//! Port of `include/timenav/vda/*.hpp`. These types are intentionally
//! partial — compatibility helpers for `timenav`'s internal model, not a
//! full schema clone.

pub mod connection;
pub mod factsheet;
pub mod instant_actions;
pub mod order;
pub mod responses;
pub mod state;
pub mod adapter;

pub use connection::{Connection, ConnectionStatus};
pub use factsheet::{Factsheet, TypeSpecification};
pub use instant_actions::{ActionBlockingType, InstantAction, InstantActions};
pub use order::{ActionReference, Order, OrderEdge, OrderNode, ResourceReservation};
pub use responses::{ActionStatus, Response};
pub use state::{BatteryState, ConnectionState, OperatingMode, ReservationState, State};
pub use adapter::{Adapter, map_robot_state, map_route_plan, try_map_route_plan};
