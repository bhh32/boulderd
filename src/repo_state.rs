use dirs;
use jwalk::WalkDir;
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
    time::Instant,
};

/// RepoState holds the state of the repositories the system is supposed to be tracking.
#[derive(Debug, Clone)]
pub struct RepoState {
    pub repo_type: Repo,
    pub packages: Vec<Package>,
    pub last_update: Instant,
}

/// Package holds the path to the package, its manifest, and monitoring file.
#[derive(Debug, Clone)]
pub struct Package {
    pub path: PathBuf,
    pub manifest: PathBuf,
    pub monitoring: PathBuf,
    pub updated: bool,
}

/// The type of repository and the path to the repository that needs to be tracked.
#[derive(Debug, Clone)]
pub enum Repo {
    Local(PathBuf),
    Volatile(PathBuf),
}

/// The default state of the Repositories that need to be tracked.
impl Default for RepoState {
    fn default() -> Self {
        // The current default is the local repository.
        let local_repo = dirs::home_dir()
            .expect("Failed to get home directory")
            .join("repos/aerynos/recipes");

        // Get all of the packages that the repo holds.
        let packages = WalkDir::new(&local_repo.to_string_lossy().to_string())
            .min_depth(1)
            .max_depth(2)
            .into_iter()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if entry.file_type().is_dir() {
                    let path = entry.path().to_path_buf();
                    let manifest = path.join("stone.yaml");
                    let monitoring = path.join("monitoring.yaml");

                    // Only include if both manifest and monitoring files exist
                    if manifest.exists() && monitoring.exists() {
                        Some(Package {
                            path,
                            manifest,
                            monitoring,
                            updated: false,
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect::<Vec<Package>>();

        // Return the RepoState of the default repo.
        Self {
            repo_type: Repo::Local(PathBuf::from(
                local_repo
                    .to_str()
                    .take()
                    .expect("Failed to get local repo path"),
            )),
            packages,
            last_update: Instant::now(),
        }
    }
}

impl RepoState {
    pub fn new(repo_type: Repo) -> Self {
        match &repo_type {
            Repo::Local(local_path) => {
                let packages: Vec<Package> = WalkDir::new(local_path.as_path())
                    .into_iter()
                    .filter(|entry| entry.is_ok())
                    .map(|entry| {
                        let entry = entry.unwrap();
                        let path = entry.path().to_path_buf();
                        let manifest = path.join("stone.yaml");
                        let monitoring = path.join("monitoring.yaml");

                        Package {
                            path,
                            manifest,
                            monitoring,
                            updated: false,
                        }
                    })
                    .collect();

                Self {
                    repo_type,
                    packages,
                    last_update: Instant::now(),
                }
            }
            Repo::Volatile(vol_path) => {
                let packages: Vec<Package> = WalkDir::new(vol_path.as_path())
                    .into_iter()
                    .filter(|entry| entry.is_ok())
                    .map(|entry| {
                        let entry = entry.unwrap();
                        let path = PathBuf::from(entry.file_name());
                        let manifest = path.join("stone.yaml");
                        let monitoring = path.join("monitoring.yaml");

                        Package {
                            path,
                            manifest,
                            monitoring,
                            updated: false,
                        }
                    })
                    .collect();

                Self {
                    repo_type,
                    packages,
                    last_update: Instant::now(),
                }
            }
        }
    }

    pub fn new_local() -> Self {
        Self::default()
    }

    pub fn new_volatile() -> Self {
        let vol_path = PathBuf::new(); // TODO: Make this the path to the volatile repo

        Self {
            repo_type: Repo::Volatile(vol_path),
            packages: Vec::new(),
            last_update: Instant::now(),
        }
    }
}

impl Package {
    pub fn update(&self) -> Result<(), Box<dyn std::error::Error>> {
        if !self.updated {
            let monitoring_content =
                fs::read_to_string(&format!("{}", self.monitoring.to_str().unwrap())).unwrap();
            let manifest_content =
                fs::read_to_string(&format!("{}", self.manifest.to_str().unwrap())).unwrap();

            let monitoring: Monitoring = serde_yaml::from_str(&monitoring_content).unwrap();
            let manifest: Manifest = serde_yaml::from_str(&manifest_content).unwrap();

            let cur_vers = manifest.version;
            let cur_hash = manifest
                .upstreams
                .as_ref()
                .and_then(|upstreams| upstreams.first())
                .map(|entry| match &entry.value {
                    UpstreamValue::Simple(hash) | UpstreamValue::Git(hash) => hash.clone(),
                    UpstreamValue::Extended { hash, .. } => hash.clone(),
                })
                .unwrap_or_default();

            let (latest_vers, latest_hash_or_url) = if let Some(first_upstream) = manifest
                .upstreams
                .as_ref()
                .and_then(|upstream| upstream.first())
            {
                self.get_latest(&monitoring, first_upstream, &cur_vers)
            } else {
                (String::new(), String::new())
            };

            // Early return if we couldn't get a latest_hash_or_url value
            if latest_hash_or_url.is_empty() {
                eprintln!(
                    "Skipping {} - no hash or url available",
                    self.path.file_name().unwrap().to_string_lossy().to_string()
                );
                return Ok(());
            }

            // Compare the current version, current hash, latest version, latest hash
            if latest_vers != cur_vers
                || serde_yaml::Value::String(latest_hash_or_url.clone()) != cur_hash
            {
                let upstream_arg = manifest
                    .upstreams
                    .as_ref()
                    .and_then(|upstreams| upstreams.first())
                    .map(|entry| match get_version_source_type(entry) {
                        VersionSourceType::Git => format!("{}, {latest_hash_or_url}", entry.url),
                        VersionSourceType::Archive => latest_hash_or_url.clone(),
                    })
                    .filter(|arg| !arg.is_empty())
                    .unwrap_or_else(|| {
                        eprintln!(
                            "Failed update for {} - no valid upstream",
                            self.path.display()
                        );
                        String::new()
                    });

                if upstream_arg.is_empty() {
                    return Ok(());
                }

                let mut boulder_cmd = Command::new("boulder")
                    .args([
                        "recipe",
                        "update",
                        "--ver",
                        &format!("{latest_vers}"),
                        "--upstream",
                        &upstream_arg,
                        "stone.yaml",
                        "-w",
                        "--build",
                        "--local",
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .expect("Failed to run boulder recipe update command");

                let stdout = boulder_cmd
                    .stdout
                    .take()
                    .expect("Failed to open recipe update process stdout");
                let stdout_reader = BufReader::new(stdout);

                // Stream the recipe update command output to the terminal
                stdout_reader.lines().for_each(|line| {
                    if let Ok(line) = line {
                        println!("{line}");
                    }
                });

                let update_status = boulder_cmd
                    .wait_with_output()
                    .expect("Failed to wait for boulder to complete");

                if update_status.status.success() {
                    println!("Successfully updated package for repository");
                } else {
                    eprintln!(
                        "Update failed for: {}",
                        self.path
                            .file_name()
                            .expect("Failed to get package directory file name")
                            .to_string_lossy()
                            .to_string()
                    );

                    // Print the error from boulder's stderr
                    if !update_status.stderr.is_empty() {
                        let err = String::from_utf8_lossy(&update_status.stderr);
                        eprintln!("Error output: {err}");
                    }
                }
            } else {
                println!(
                    "Nothing to update for {}",
                    self.path
                        .file_name()
                        .expect("Failed to get package directory file name")
                        .to_string_lossy()
                        .to_string()
                );
            }
        }

        Ok(())
    }

    fn get_latest(
        &self,
        monitoring: &Monitoring,
        cur_upstream: &UpstreamEntry,
        cur_vers: &str,
    ) -> (String, String) {
        let client = reqwest::blocking::Client::new();

        // Get version from release-monitoring.org if available
        if let Some(id) = monitoring.releases.id {
            let url = format!("https://release-monitoring.org/api/project/{id}");

            if let Ok(response) = client.get(&url).send() {
                if let Ok(project) = response.json::<ApiProject>() {
                    let new_version = project.version;

                    // If version unchanged, return current
                    if new_version == cur_vers {
                        return (new_version, self.get_current_hash(cur_upstream));
                    }

                    // Get hash/URL based on source type
                    match get_version_source_type(cur_upstream) {
                        VersionSourceType::Git => (
                            new_version.clone(),
                            self.get_git_hash(&new_version, cur_upstream),
                        ),
                        VersionSourceType::Archive => {
                            let new_url =
                                replace_version_in_url(&cur_upstream.url, cur_vers, &new_version);
                            (new_version, new_url)
                        }
                    }
                } else {
                    (String::new(), String::new())
                }
            } else {
                (String::new(), String::new())
            }
        } else {
            // No monitoring ID - skip for now
            (String::new(), String::new())
        }
    }

    fn get_current_hash(&self, upstream: &UpstreamEntry) -> String {
        match &upstream.value {
            UpstreamValue::Simple(hash) | UpstreamValue::Git(hash) => hash.clone(),
            UpstreamValue::Extended { hash, .. } => hash.clone(),
        }
    }

    fn get_git_hash(&self, version: &str, upstream: &UpstreamEntry) -> String {
        // GitHub API call to get commit hash for version

        // Return String::new() if fails
        String::new()
    }
}

// Helper Functions
fn is_git_source(upstream_entry: &UpstreamEntry) -> bool {
    // Check if explicitly marked as git
    if upstream_entry.url.starts_with("git|") {
        return true;
    }

    // Check for git forges
    let git_forges = [
        "github.com",
        "gitlab.com",
        "bitbucket.com",
        "git.kernel.org",
        "code.videolan.org",
        "git.savannah.gnu.org",
        "invent.kde.org",
    ];

    git_forges
        .iter()
        .any(|forge| upstream_entry.url.contains(forge))
        && upstream_entry.url.contains(".git")
}

fn get_version_source_type(upstream_entry: &UpstreamEntry) -> VersionSourceType {
    if is_git_source(upstream_entry) {
        VersionSourceType::Git
    } else {
        VersionSourceType::Archive
    }
}

fn replace_version_in_url(url: &str, old_version: &str, new_version: &str) -> String {
    use regex::Regex;

    let patterns = [
        format!(r"v?{}", regex::escape(old_version)),
        format!(r"{}", regex::escape(old_version)),
    ];

    for pattern in &patterns {
        if let Ok(reg) = Regex::new(pattern) {
            if reg.is_match(url) {
                return reg.replace(url, new_version).to_string();
            }
        }
    }

    // Fallback in the case a pattern isn't found
    url.replace(old_version, new_version)
}

// Helper Structs and enums
#[derive(Debug, serde::Deserialize)]
enum VersionSourceType {
    Git,
    Archive,
}

#[derive(Debug, serde::Deserialize)]
struct Releases {
    id: Option<u32>,
    #[serde(default)]
    rss: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct Monitoring {
    releases: Releases,
    #[serde(default)]
    security: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone)]
enum UpstreamValue {
    Simple(String), // Direct hash string
    Extended {
        hash: String,
        properties: HashMap<String, serde_yaml::Value>,
    },
    Git(String),
}

#[derive(Debug, Clone)]
struct UpstreamEntry {
    url: String,
    value: UpstreamValue,
}

#[derive(Debug, serde::Deserialize)]
struct Manifest {
    name: String,
    version: String,
    #[serde(default)]
    release: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_upstreams")]
    upstreams: Option<Vec<UpstreamEntry>>,
    #[serde(default)]
    homepage: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ApiProject {
    version: String,
    homepage: Option<String>,
}

// Custom deserailizer for upstreams
fn deserialize_upstreams<'de, D>(deserializer: D) -> Result<Option<Vec<UpstreamEntry>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<Vec<HashMap<String, serde_yaml::Value>>> = Option::deserialize(deserializer)?;

    Ok(raw.map(|upstreams| {
        upstreams
            .into_iter()
            .filter_map(|map| {
                let (url, value) = map.into_iter().next()?;

                let upstream_value = match value {
                    // Simple string hash
                    serde_yaml::Value::String(hash) => {
                        if url.starts_with("git|") {
                            UpstreamValue::Git(hash)
                        } else {
                            UpstreamValue::Simple(hash)
                        }
                    }
                    // Extended object with hash + properties
                    serde_yaml::Value::Mapping(map) => {
                        let hash = map.get("hash")?.as_str()?.to_string();
                        let mut properties = HashMap::new();

                        for (k, v) in map {
                            if let serde_yaml::Value::String(key) = k {
                                if key != "hash" {
                                    properties.insert(key.clone(), v.clone());
                                }
                            }
                        }

                        if url.starts_with("git|") {
                            UpstreamValue::Git(hash)
                        } else {
                            UpstreamValue::Extended { hash, properties }
                        }
                    }
                    _ => return None,
                };

                Some(UpstreamEntry {
                    url,
                    value: upstream_value,
                })
            })
            .collect()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_default_repo_state() {
        let def_rs = RepoState::default();

        match def_rs.repo_type {
            Repo::Local(local_repo) => {
                assert_eq!(
                    local_repo,
                    dirs::home_dir()
                        .expect("Failed test to get home_dir")
                        .join("repos/aerynos/recipes")
                );
            }
            _ => panic!(
                "RepoState::default() did not default to the Repo::Local type with the local repo at /home/$USER/repos/aerynos/recipes directory"
            ),
        }

        assert!(!def_rs.packages.is_empty());
        assert!(def_rs.last_update < std::time::Instant::now());
    }

    #[test]
    fn test_create_local_repo_state() {
        let local_repo = RepoState::new_local();

        match local_repo.repo_type {
            Repo::Local(repo) => {
                assert_eq!(
                    repo,
                    dirs::home_dir()
                        .expect("Failed to get home directory")
                        .join("repos/aerynos/recipes")
                );
            }
            _ => panic!(
                "RepoState::default() did not default to the Repo::Local type with the local repo at /home/$USER/repos/aerynos"
            ),
        }

        assert!(!local_repo.packages.is_empty());
        assert!(local_repo.last_update < std::time::Instant::now());
    }

    #[test]
    fn test_get_package_latest_info() {
        // Create a test Package pointing to the cosmic-applets local directory
        let cosmic_applets_path = dirs::home_dir()
            .expect("Failed to get home directory")
            .join("repos/aerynos/recipes/c/cosmic-applets");

        let package = Package {
            path: cosmic_applets_path.clone(),
            manifest: cosmic_applets_path.join("stone.yaml"),
            monitoring: cosmic_applets_path.join("monitoring.yaml"),
            updated: false,
        };

        // Verify the files exist before testing
        assert!(
            package.manifest.exists(),
            "stone.yaml file should exist for cosmic-applets"
        );
        assert!(
            package.monitoring.exists(),
            "monitoring.yaml file should exist for cosmic-applets"
        );

        // Read and parse the monitoring file to create a Monitoring struct
        let monitoring_content = fs::read_to_string(&package.monitoring)
            .expect("Should be able to read monitoring.yaml");
        let monitoring: Monitoring = serde_yaml::from_str(&monitoring_content)
            .expect("Should be able to parse monitoring.yaml");

        let manifest_content =
            fs::read_to_string(&package.manifest).expect("Should be able to read stone.yaml");
        let manifest: Manifest =
            serde_yaml::from_str(&manifest_content).expect("Failed to parse content of stone.yaml");

        // Test get_latest
        if let Some(first_upstream) = manifest
            .upstreams
            .as_ref()
            .and_then(|upstream| upstream.first())
        {
            let (latest_vers, latest_hash) =
                package.get_latest(&monitoring, first_upstream, &manifest.version);

            // Assertions
            assert!(
                !latest_vers.is_empty(),
                "Latest version should not be empty"
            );

            // For cosmic-applets, it's expected to be a GitHub project, so a hash should be found
            assert!(
                !latest_hash.is_empty(),
                "Latest hash should not be empty for cosmic-applets"
            );

            // Verify that monitoring ID is correct for cosmic-applets (377113)
            assert_eq!(
                monitoring.releases.id,
                Some(377113),
                "cosmic-applets should have a monitoring ID 377113"
            );
        }
    }
}
