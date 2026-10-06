fn main() -> anyhow::Result<()> {
    demo_writer::run_from(std::env::args_os().collect())
}
