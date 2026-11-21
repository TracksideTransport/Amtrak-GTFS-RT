//! Process entry for `amtrak-gtfs-rt-service`, the one service in the one container.
//!
//! The first argument `refresh-station-names` asks the running service to rename stops now (see
//! [`amtrak_gtfs_rt_service::request_station_names_refresh`]). Any other arguments start the
//! service. A fatal error is logged at `ERROR` with its stage and cause chain, and the process
//! exits non-zero (see [`amtrak_gtfs_rt_service::exit_code`]).

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("refresh-station-names"))
    {
        return amtrak_gtfs_rt_service::request_station_names_refresh();
    }
    amtrak_gtfs_rt_service::exit_code(amtrak_gtfs_rt_service::run_service().await)
}
