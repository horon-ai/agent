/// Inspect generated images recorded in one session.
#[derive(clap::Args, Debug)]
pub(crate) struct ImagesArgs {
    /// Session that generated the images
    #[arg(long, value_name = "ID")]
    pub(crate) session: String,

    /// Emit JSON instead of plain text
    #[arg(long)]
    pub(crate) json: bool,

    /// Copy image NUMBER from the list to DEST, which must not exist yet
    #[arg(long, num_args = 2, value_names = ["NUMBER", "DEST"])]
    pub(crate) copy: Option<Vec<String>>,
}
