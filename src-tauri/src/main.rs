// Cache le terminal Windows en release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    velmora_desktop_lib::run()
}
