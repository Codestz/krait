pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

pub struct Config {
    pub name: String,
    pub port: u16,
}

impl Config {
    pub fn new(name: String, port: u16) -> Self {
        Self { name, port }
    }

    pub fn validate(&self) -> bool {
        self.port > 0
    }
}
