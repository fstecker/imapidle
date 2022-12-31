use anyhow::Result as AResult;

fn main() -> AResult<()> {
	if let Err(err) = imapidle::run() {
		println!("{err:?}");
		Err(err)
	} else {
		Ok(())
	}
}
