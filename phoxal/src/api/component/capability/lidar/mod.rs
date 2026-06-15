pub mod v1;

contract! {
    pub enum Scan {
        V1(v1::Scan),
    }
}
