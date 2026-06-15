pub mod v1;

contract! {
    pub enum Target {
        V1(v1::Target),
    }
}

contract! {
    pub enum State {
        V1(v1::State),
    }
}

contract! {
    pub enum ActuatorCommands {
        V1(v1::ActuatorCommands),
    }
}

contract! {
    pub enum Saturation {
        V1(v1::Saturation),
    }
}

contract! {
    #[derive(Eq)]
    pub enum Watchdog {
        V1(v1::Watchdog),
    }
}

contract! {
    pub enum Kinematics {
        V1(v1::Kinematics),
    }
}
