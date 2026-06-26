use zellij_tile::prelude::*;
use std::collections::BTreeMap;

mod status;

#[derive(Default)]
struct State;

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _config: BTreeMap<String, String>) {}
    fn update(&mut self, _event: Event) -> bool {
        false
    }
    fn render(&mut self, _rows: usize, _cols: usize) {
        print!("agents");
    }
}
