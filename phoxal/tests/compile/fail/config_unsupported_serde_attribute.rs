#[derive(serde::Deserialize, phoxal::Config)]
struct Nested {
    value: String,
}

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {
    #[serde(flatten)]
    nested: Nested,
}

fn main() {}
