use phoxal_macros::Api;

#[derive(Api)]
struct Unsupported {
    ignored_before_issue_941: String,
}

fn main() {}
