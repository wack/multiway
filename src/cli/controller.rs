use miette::Result;

/// Run the API Gateway Controller
pub struct Controller;

impl Controller {
    pub fn new() -> Self {
        Self
    }

    pub fn dispatch(self) -> Result<()> {
        println!("Hello World");
        Ok(())
    }
}
