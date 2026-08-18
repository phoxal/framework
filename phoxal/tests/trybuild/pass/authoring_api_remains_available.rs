use phoxal::api;

fn main() {
    let _ = api::topics().drive().state().client();
}
