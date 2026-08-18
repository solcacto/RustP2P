pub struct HostState {
    counter: u32,
    name: String,
}

impl HostState {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            counter: 0,
            name: name.into(),
        }
    }

    pub fn increment_counter(&mut self) {
        self.counter += 1;
    }

    pub fn counter(&self) -> u32 {
        self.counter
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}