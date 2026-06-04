// Prevents additional console window on Windows in release, DO NOT REMOVE!!
// TEMPORARY: disabled to see xECM debug output
// #![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    llm_wiki_lib::run();
}
