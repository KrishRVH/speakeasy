//! Owned-window checks compare completed preview, copy payload, and explicit video export pixels.

use super::*;

#[derive(Default)]
struct Copies(Vec<crate::show::copy::Request>);
impl gpui::Global for Copies {}

#[expect(
    clippy::future_not_send,
    reason = "Owned native review runs on GPUI's UI thread"
)]
pub(super) async fn exercise(
    window: gpui::WindowHandle<Review>,
    original: &std::path::Path,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    let directory = original.with_file_name("20261020T143206Z");
    fs::create_dir(&directory)?;
    movie::tests::native_geometry_fixture(&directory)?;
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("show.json"))?)?;
    *manifest
        .pointer_mut("/marks/0/span")
        .context("Synthetic pin mark is missing")? = serde_json::json!([280, 280]);
    manifest.as_object_mut().context("Synthetic manifest must be an object")?.extend([
        ("pins".into(), serde_json::json!([{"at":280,"point":[0.5,0.5]}])),
        ("spaces".into(), serde_json::json!({"strokes":[],"pins":[{"generation":1,"part":[0,0.25,1,0.75],"observed":283}]})),
    ]);
    fs::write(directory.join("show.json"), serde_json::to_vec(&manifest)?)?;
    cx.update(|cx| cx.set_global(Copies::default()))?;
    window.update(cx, |view, window, cx| {
        view.actions.can_copy = |_| true;
        view.actions.copy = |request, cx| {
            cx.global_mut::<Copies>().0.push(request);
            Ok(Box::pin(async { Ok(crate::show::copy::Outcome::Copied) }))
        };
        let key = view.library.select(directory.clone(), 1);
        view.open_selected(key, None, Some(window), cx);
        cx.notify();
    })?;
    wait_for(window, ready, cx).await?;
    let annotated = window.read_with(cx, |view, _| -> anyhow::Result<_> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing annotated sample")
        };
        assert_eq!(decoded.at, Duration::from_nanos(283_333_300));
        assert_eq!(
            decoded.annotations,
            movie::AnnotationState::Painted {
                drawn: 1,
                omitted: 0
            }
        );
        Ok(Arc::clone(&decoded.image))
    })??;
    window.update(cx, |view, _, cx| view.copy(CopyKind::Moment, cx))?;
    wait_for(window, |view| view.copying.is_none(), cx).await?;
    let copied = cx
        .update(|cx| cx.global_mut::<Copies>().0.pop())?
        .context("Missing copy payload")?;
    let crate::show::copy::Request::Moment { text, image } = copied else {
        anyhow::bail!("Wrong copy payload")
    };
    assert!(Arc::ptr_eq(&image, &annotated));
    assert!(text.contains("--video --annotated --at 0:00.284"));
    assert!(!text.contains("PRIVATE"));
    drop(image);
    let output = directory.join("annotated.png");
    let args = [
        "frame".into(),
        directory.clone().into_os_string(),
        "--video".into(),
        "--annotated".into(),
        "--at".into(),
        "0:00.284".into(),
        "--out".into(),
        output.clone().into_os_string(),
        "--json".into(),
    ];
    cx.background_executor()
        .spawn(async move { crate::show::command::Command::parse(args.into_iter())?.run() })
        .await?;
    let mut png = image::load_from_memory(&fs::read(output)?)?
        .into_rgba8()
        .into_raw();
    for pixel in png.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    assert_eq!(annotated.as_bytes(0), Some(png.as_slice()));
    window.update(cx, |view, window, cx| {
        let selected = view.library.selected.clone();
        view.act(Action::Annotations, window, cx);
        assert_ne!(view.library.selected, selected);
        assert!(view.loading.is_some());
        assert!(view.video_caption().contains("Annotations on"));
        view.copy(CopyKind::Moment, cx);
        assert!(cx.global::<Copies>().0.is_empty());
    })?;
    wait_for(window, ready, cx).await?;
    window.read_with(cx, |view, _| -> anyhow::Result<()> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing raw sample")
        };
        assert_eq!(decoded.annotations, movie::AnnotationState::Off);
        assert_ne!(decoded.image.as_bytes(0), annotated.as_bytes(0));
        assert!(view.video_caption().contains("annotations off"));
        Ok(())
    })??;
    window.update(cx, |view, window, cx| {
        for index in 0..15 {
            view.act(Action::Frame(index), window, cx);
            view.act(Action::Annotations, window, cx);
        }
        view.act(Action::Frame(1), window, cx);
    })?;
    wait_for(window, ready, cx).await?;
    window.read_with(cx, |view, _| -> anyhow::Result<()> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing final annotated sample")
        };
        assert!(view.annotated);
        assert!(decoded.annotations.painted());
        assert_eq!(decoded.image.as_bytes(0), annotated.as_bytes(0));
        Ok(())
    })??;
    window.update(cx, |view, window, cx| {
        view.act(Action::Frame(29), window, cx);
    })?;
    wait_for(window, ready, cx).await?;
    window.read_with(cx, |view, _| -> anyhow::Result<()> {
        let ImageState::Ready(decoded) = &view.image else {
            anyhow::bail!("Missing recovered tail")
        };
        assert_eq!(
            decoded.annotations,
            movie::AnnotationState::BeyondCoverage {
                through: speakeasy_show::Moment::from_millis(1000)
            }
        );
        assert!(view.video_caption().contains("showing original video"));
        Ok(())
    })??;
    Ok(())
}
