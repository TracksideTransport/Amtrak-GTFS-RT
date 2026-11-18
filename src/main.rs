//! Process entry for `amtrak-gtfs-rt-service`, the one service in the one container.
//!
//! A fatal error is logged at `ERROR` with its stage and cause chain, and the process exits
//! non-zero (see [`amtrak_gtfs_rt_service::exit_code`]).

#[tokio::main]
async fn main() -> std::process::ExitCode {
    amtrak_gtfs_rt_service::exit_code(amtrak_gtfs_rt_service::run_service().await)
}
