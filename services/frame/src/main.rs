mod config;
mod frame;
mod ring_buffer;
mod transform;

fn main() -> phoxal::Result<()> {
    phoxal::run::<frame::Frame>()
}
