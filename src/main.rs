pub mod logic;
pub mod repo_state;

use logic::update_cosmic_packages;

fn main() {
    update_cosmic_packages();
}
