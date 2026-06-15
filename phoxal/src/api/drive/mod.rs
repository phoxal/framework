pub mod v1;

contract! {
    pub enum Target {
        "1" => V1(v1::Target),
    }
}

contract! {
    pub enum State {
        "1" => V1(v1::State),
    }
}

contract! {
    pub enum ActuatorCommands {
        "1" => V1(v1::ActuatorCommands),
    }
}

contract! {
    pub enum Saturation {
        "1" => V1(v1::Saturation),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Watchdog {
        "1" => V1(v1::Watchdog),
    }
}

contract! {
    pub enum Kinematics {
        "1" => V1(v1::Kinematics),
    }
}
