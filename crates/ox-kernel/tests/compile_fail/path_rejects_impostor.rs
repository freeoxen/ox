use ox_kernel::path;

struct PretendComponent;
impl PretendComponent {
    fn validated_str(&self) -> &str { "bad-name" }
}

fn main() {
    let _ = path!("safe", PretendComponent);
}
