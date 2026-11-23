//! Program desktop aplikasi pemain CloudPC. Semua logika ada di pustaka (`lib.rs`), yang juga dimuat aplikasi Android.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

fn main() {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    cloudpc_client_lib::desktop_main();
}
