fn main() {
    // The web interface is built by `npm run build` in ui/ (see README); Tauri embeds ui/dist.
    tauri_build::build()
}
