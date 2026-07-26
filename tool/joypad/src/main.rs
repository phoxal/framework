mod joypad;

fn main() -> phoxal::Result<()> {
    phoxal::run::<joypad::ToolJoypad>()
}
