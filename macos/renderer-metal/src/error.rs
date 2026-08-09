use core::fmt;

/// Why the renderer stopped.
///
/// Almost every variant is a startup failure. The exception,
/// [`RendererError::TextureBind`], is fatal on purpose: it means a frame
/// arrived that is not an IOSurface-backed NV12 buffer, so the contract with
/// the decoder is broken and every later frame would fail the same way.
#[derive(Debug)]
pub enum RendererError {
    /// `run` was called off the main thread, where AppKit refuses to work.
    NotMainThread,
    NoMetalDevice,
    NoScreen,
    NoCommandQueue,
    ShaderCompile(String),
    /// The compiled library is missing a function the pipeline needs.
    MissingShaderFunction(&'static str),
    PipelineCreate(String),
    TextureCacheCreate(i32),
    /// A decoded frame could not be aliased as a Metal texture.
    TextureBind {
        plane: usize,
        status: i32,
    },
    /// The preflight found the window or display in a state that would make
    /// the measurement meaningless, and the caller asked to be told rather
    /// than handed a number it cannot trust.
    DirtyEnvironment(Vec<String>),
}

impl fmt::Display for RendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RendererError::NotMainThread => f.write_str("the renderer must run on the main thread"),
            RendererError::NoMetalDevice => f.write_str("no Metal device available"),
            RendererError::NoScreen => f.write_str("no display attached"),
            RendererError::NoCommandQueue => f.write_str("could not create a Metal command queue"),
            RendererError::ShaderCompile(message) => {
                write!(f, "shader compilation failed: {message}")
            }
            RendererError::MissingShaderFunction(name) => {
                write!(f, "shader library has no function `{name}`")
            }
            RendererError::PipelineCreate(message) => {
                write!(f, "render pipeline creation failed: {message}")
            }
            RendererError::TextureCacheCreate(status) => {
                write!(f, "CVMetalTextureCacheCreate failed with {status}")
            }
            RendererError::TextureBind { plane, status } => {
                write!(
                    f,
                    "plane {plane} could not be bound as a Metal texture ({status})"
                )
            }
            RendererError::DirtyEnvironment(problems) => {
                write!(f, "the environment is unfit for a measurement: ")?;
                for (index, problem) in problems.iter().enumerate() {
                    if index > 0 {
                        f.write_str("; ")?;
                    }
                    f.write_str(problem)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for RendererError {}
