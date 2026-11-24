mod archive;
mod download;
mod engine;

use std::{
    convert::Infallible,
    io,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use anyhow::anyhow;
use speakeasy_platform::StoppableThread;
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::watch,
};

use self::{
    download::{Download, fetch},
    engine::choose_engine,
};
use crate::{
    child::kill_and_reap,
    config::{Config, Engine},
    http::{Trust, client_builder},
    models::Model,
    progress::Progress,
};

const PARAKEET: Download<'static> = Download {
    url: "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.q8_0.gguf",
    size: 713_975_456,
    sha256: "e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e",
};

const NEMOTRON: Download<'static> = Download {
    url: "https://huggingface.co/nvidia/nemotron-3.5-asr-streaming-0.6b/resolve/ea30d66debe3740a08b573244286791d423d6b3e/nemotron-3.5-asr-streaming-0.6b.q8_0.gguf",
    size: 742_090_464,
    sha256: "3fc991d3badad7277c11030a7519832cddaf2057aafed6d4b25147e953a070b1",
};

const CHECKING: Progress = Progress::Step("Checking this machine");

const fn source(model: Model) -> (&'static Download<'static>, &'static str) {
    match model {
        Model::Parakeet => (&PARAKEET, "Downloading Parakeet v3"),
        Model::Nemotron => (&NEMOTRON, "Downloading Nemotron 3.5"),
    }
}

pub(crate) fn downloaded() -> Vec<Model> {
    let Ok(root) = crate::paths::data_root() else {
        return Vec::new();
    };
    Model::ALL
        .into_iter()
        .filter(|&model| {
            let (download, _) = source(model);
            model_directory(&root, download)
                .join(download.file_name())
                .is_file()
        })
        .collect()
}

fn model_directory(root: &Path, download: &Download<'_>) -> PathBuf {
    root.join("models").join(download.sha256)
}

pub(crate) struct Installed {
    choice: Model,
    engine_executable: PathBuf,
    model: PathBuf,
    use_gpu: bool,
}

impl Installed {
    pub(crate) fn apply_to(self, config: &mut Config) {
        config.engine = Engine::Nemo;
        config.nemo_model = Some(self.choice);
        if self.choice == Model::Nemotron {
            config.language = crate::models::nemotron_language(&config.language).into();
        }
        config.engine_executable = self.engine_executable;
        config.model = self.model;
        config.use_gpu &= self.use_gpu;
    }
}

pub(crate) struct Setup {
    thread: StoppableThread,
    updates: watch::Receiver<Progress>,
    result: async_channel::Receiver<anyhow::Result<Installed>>,
}

impl Setup {
    pub(crate) fn start(choice: Model) -> anyhow::Result<Self> {
        Self::start_in(
            crate::paths::data_root()?,
            async move |root, progress, installation| {
                install(root, progress, installation, choice).await
            },
        )
    }

    fn start_in(
        root: PathBuf,
        work: impl AsyncFnOnce(
            &Path,
            &watch::Sender<Progress>,
            &mut Installation,
        ) -> anyhow::Result<Installed>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        let (progress, updates) = watch::channel(CHECKING);
        let (finish, result) = async_channel::bounded(1);
        let thread = StoppableThread::spawn("setup", move |cancelled| {
            let outcome = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime
                    .block_on(Installation::run(&cancelled, async |installation| {
                        work(&root, &progress, installation).await
                    })),
                Err(error) => Some(Err(error.into())),
            };
            if let Some(result) = outcome {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A released owner no longer wants the result; native cleanup has already finished"
                )]
                let _ = finish.try_send(result);
            }
        })?;
        Ok(Self {
            thread,
            updates,
            result,
        })
    }

    pub(crate) fn progress(&self) -> Progress {
        *self.updates.borrow()
    }

    pub(crate) fn progress_changes(&self) -> watch::Receiver<Progress> {
        self.updates.clone()
    }

    pub(crate) fn outcome(
        &self,
    ) -> impl Future<Output = Option<anyhow::Result<Installed>>> + use<> {
        let result = self.result.clone();
        let stopped = self.stopped();
        async move {
            let outcome = result.recv().await.ok();
            stopped.await;
            outcome
        }
    }

    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + use<> {
        self.thread.stopped()
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    pub(crate) fn request_stop(&self) {
        self.thread.request_stop();
    }
}

#[derive(Default)]
struct Installation {
    child: Option<Child>,
}

impl Installation {
    async fn run<T>(
        cancelled: &async_channel::Receiver<Infallible>,
        work: impl AsyncFnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> Option<anyhow::Result<T>> {
        let mut installation = Self::default();
        let result = tokio::select! {
            biased;
            _ = cancelled.recv() => None,
            result = work(&mut installation) => Some(result),
        };
        installation.stop().await;
        result
    }

    async fn output(&mut self, command: &mut Command) -> anyhow::Result<(ExitStatus, Vec<u8>)> {
        let child = self.child.insert(command.spawn()?);
        let mut pipe = child.stdout.take();
        let output = tokio::try_join!(child.wait(), async {
            let mut bytes = Vec::new();
            if let Some(pipe) = &mut pipe {
                pipe.read_to_end(&mut bytes).await?;
            }
            Ok::<_, io::Error>(bytes)
        })?;
        self.child = None;
        Ok(output)
    }

    async fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            kill_and_reap(child).await;
        }
        self.child = None;
    }
}

async fn install(
    root: &Path,
    progress: &watch::Sender<Progress>,
    installation: &mut Installation,
    choice: Model,
) -> anyhow::Result<Installed> {
    let client = client_builder(Trust::System)?
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()?;
    let engine = choose_engine(&client, root, progress, installation).await?;
    let (download, step) = source(choice);
    let model = fetch(
        &client,
        download,
        &model_directory(root, download),
        step,
        progress,
    )
    .await?;
    Ok(Installed {
        choice,
        engine_executable: engine.executable(),
        model,
        use_gpu: engine.use_gpu,
    })
}

fn file_error(directory: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| {
        anyhow!(
            "Setup can't use {}: {error}. {}",
            directory.display(),
            crate::paths::remedy(&error)
        )
    }
}

fn system32() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32")
}

#[cfg(test)]
mod tests;
