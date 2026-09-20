//! Collects what the daemon currently has, shared by backup and doctor.

use crate::application::ports::DockerPort;
use crate::domain::error::AppResult;
use crate::domain::plan::Inventory;

pub fn collect_inventory(docker: &dyn DockerPort) -> AppResult<Inventory> {
    Ok(Inventory {
        volumes: docker.list_volumes()?,
        images: docker.list_images()?,
        containers: docker.list_containers()?,
    })
}
