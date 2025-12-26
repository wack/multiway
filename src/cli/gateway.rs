use miette::Result;

/// Run the API Gateway
pub struct Gateway;

impl Gateway {
    pub fn new() -> Self {
        Self
    }

    pub fn dispatch(self) -> Result<()> {
        println!("Hello world");
        Ok(())
    }
}
