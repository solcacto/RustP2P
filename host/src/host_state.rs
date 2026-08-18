use std::time::Instant;

pub struct HostState {
    counter: u32,
    name: String,
    frame_count: u64,
    last_frame_time: Instant,
    delta_time: f64,
}

impl HostState {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            counter: 0,
            name: name.into(),
            frame_count: 0,
            last_frame_time: Instant::now(),
            delta_time: 0.0,
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

    pub fn update_frame(&mut self) -> f64 {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame_time).as_secs_f64();
        self.delta_time = delta;
        self.frame_count += 1;
        self.last_frame_time = now;
        delta
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn delta_time(&self) -> f64 {
        self.delta_time
    }
}
