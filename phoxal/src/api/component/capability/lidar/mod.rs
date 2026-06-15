pub mod v1;

contract! {
    pub enum Scan {
        "1" => V1(v1::Scan),
    }
}
