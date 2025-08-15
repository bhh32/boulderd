use crate::repo_state::RepoState;
use std::thread;

pub fn update_cosmic_packages() {
    let local_state = RepoState::new_local();

    local_state.packages.into_iter().for_each(|package| {
        thread::scope(|s| {
            s.spawn(move || {
                if let Err(e) = package.update() {
                    eprintln!("{e}");
                }
            });
        });
    });
}
