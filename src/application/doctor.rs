//! Doctor use case: describe the daemon and check for the external tools we shell out to.

use crate::application::inventory::collect_inventory;
use crate::application::ports::{DockerPort, ToolLocator};
use crate::domain::error::AppResult;
use crate::domain::plan::Inventory;
use crate::domain::refs::ImageOrigin;
use crate::domain::report::{ContainerCounts, DoctorReport, ImageCounts, ToolStatus, VolumeCounts};

pub const REQUIRED_TOOLS: [&str; 2] = ["docker", "tar"];
pub const OPTIONAL_TOOLS: [&str; 1] = ["bzip2"];

pub struct DoctorService<'a> {
    pub docker: &'a dyn DockerPort,
    pub tools: &'a dyn ToolLocator,
}

impl DoctorService<'_> {
    pub fn run(&self) -> AppResult<DoctorReport> {
        let (docker, docker_error, inventory) = match self.docker.engine_info() {
            Ok(info) => (Some(info), None, collect_inventory(self.docker)?),
            Err(error) => (None, Some(error.to_string()), Inventory::default()),
        };

        let created = inventory
            .images
            .iter()
            .filter(|i| i.origin == ImageOrigin::Built)
            .count();
        let volatile = inventory.volumes.iter().filter(|v| v.volatile).count();
        let running = inventory.containers.iter().filter(|c| c.running).count();

        let tools = REQUIRED_TOOLS
            .iter()
            .map(|name| (*name, true))
            .chain(OPTIONAL_TOOLS.iter().map(|name| (*name, false)))
            .map(|(name, required)| ToolStatus {
                name: name.to_string(),
                required,
                info: self.tools.locate(name),
            })
            .collect();

        Ok(DoctorReport {
            docker,
            docker_error,
            images: ImageCounts {
                created,
                pulled: inventory.images.len() - created,
                total: inventory.images.len(),
            },
            volumes: VolumeCounts {
                named: inventory.volumes.len() - volatile,
                volatile,
                total: inventory.volumes.len(),
            },
            containers: ContainerCounts {
                running,
                total: inventory.containers.len(),
            },
            tools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::fakes::{FakeDocker, FakeTools};
    use crate::domain::refs::ImageOrigin;

    #[test]
    fn doctor_counts_everything_and_checks_tools() {
        let docker = FakeDocker::default()
            .with_volume("pgdata", b"")
            .with_volatile_volume(&"0c".repeat(32), b"")
            .with_image("app:latest", ImageOrigin::Built, b"")
            .with_image("nginx:alpine", ImageOrigin::Pulled, b"")
            .with_image("redis:7", ImageOrigin::Pulled, b"")
            .with_container("web", "nginx:alpine", b"");
        let tools = FakeTools::default().with("docker").with("tar");
        let report = DoctorService {
            docker: &docker,
            tools: &tools,
        }
        .run()
        .unwrap();
        assert_eq!(report.docker.as_ref().unwrap().server_version, "29.5.2");
        assert_eq!(
            report.images,
            ImageCounts {
                created: 1,
                pulled: 2,
                total: 3
            }
        );
        assert_eq!(
            report.volumes,
            VolumeCounts {
                named: 1,
                volatile: 1,
                total: 2
            }
        );
        assert_eq!(
            report.containers,
            ContainerCounts {
                running: 1,
                total: 1
            }
        );
        let names: Vec<(&str, bool, bool)> = report
            .tools
            .iter()
            .map(|t| (t.name.as_str(), t.required, t.info.is_some()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("docker", true, true),
                ("tar", true, true),
                ("bzip2", false, false)
            ]
        );
        assert!(report.is_healthy());
    }

    #[test]
    fn doctor_survives_an_unavailable_daemon() {
        let docker = FakeDocker::unavailable();
        let tools = FakeTools::default()
            .with("docker")
            .with("tar")
            .with("bzip2");
        let report = DoctorService {
            docker: &docker,
            tools: &tools,
        }
        .run()
        .unwrap();
        assert!(report.docker.is_none());
        assert!(
            report
                .docker_error
                .as_deref()
                .unwrap()
                .contains("fake daemon down")
        );
        assert_eq!(report.images.total, 0);
        assert!(!report.is_healthy());
        assert_eq!(report.exit_code(), 1);
    }
}
