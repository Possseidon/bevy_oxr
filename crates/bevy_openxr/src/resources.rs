use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::XrError;
use crate::graphics::*;
use crate::layer_builder::CompositionLayer;
use crate::types::*;
use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use openxr::AnyGraphics;

#[derive(Deref, Clone)]
pub struct XrEntry(pub openxr::Entry);

impl XrEntry {
    pub fn enumerate_extensions(&self) -> Result<XrExtensions> {
        Ok(self.0.enumerate_extensions().map(Into::into)?)
    }

    pub fn create_instance(
        &self,
        app_info: AppInfo,
        exts: XrExtensions,
        layers: &[&str],
        backend: GraphicsBackend,
    ) -> Result<XrInstance> {
        let available_exts = self.enumerate_extensions()?;

        if !backend.is_available(&available_exts) {
            return Err(XrError::UnavailableBackend(backend));
        }

        let required_exts = exts | backend.required_exts();

        let instance = self.0.create_instance(
            &openxr::ApplicationInfo {
                application_name: &app_info.name,
                application_version: app_info.version.to_u32(),
                engine_name: "Bevy",
                engine_version: Version::BEVY.to_u32(),
            },
            &required_exts.into(),
            layers,
        )?;

        Ok(XrInstance(instance, backend, app_info))
    }

    pub fn available_backends(&self) -> Result<Vec<GraphicsBackend>> {
        Ok(GraphicsBackend::available_backends(
            &self.enumerate_extensions()?,
        ))
    }
}

#[derive(Resource, Deref, Clone)]
pub struct XrInstance(
    #[deref] pub openxr::Instance,
    pub(crate) GraphicsBackend,
    pub(crate) AppInfo,
);

impl XrInstance {
    pub fn init_graphics(
        &self,
        system_id: openxr::SystemId,
    ) -> Result<(WgpuGraphics, XrSessionGraphicsInfo)> {
        graphics_match!(
            self.1;
            _ => {
                let (graphics, session_info) = Api::init_graphics(&self.2, &self, system_id)?;

                Ok((graphics, XrSessionGraphicsInfo(Api::wrap(session_info))))
            }
        )
    }

    /// # Safety
    ///
    /// `info` must contain valid handles for the graphics api
    pub unsafe fn create_session(
        &self,
        system_id: openxr::SystemId,
        info: XrSessionGraphicsInfo,
    ) -> Result<(XrSession, XrFrameWaiter, XrFrameStream)> {
        if !info.0.using_graphics_of_val(&self.1) {
            return Err(XrError::GraphicsBackendMismatch {
                item: std::any::type_name::<XrSessionGraphicsInfo>(),
                backend: info.0.graphics_name(),
                expected_backend: self.1.graphics_name(),
            });
        }
        graphics_match!(
            info.0;
            info => {
                let (session, frame_waiter, frame_stream) = self.0.create_session::<Api>(system_id, &info)?;
                Ok((session.into(), XrFrameWaiter(frame_waiter), XrFrameStream(Api::wrap(Arc::new(Mutex::new(frame_stream))))))
            }
        )
    }
}

#[derive(Clone)]
pub struct XrSessionGraphicsInfo(pub(crate) GraphicsWrap<Self>);

impl GraphicsType for XrSessionGraphicsInfo {
    type Inner<G: GraphicsExt> = G::SessionCreateInfo;
}

#[derive(Resource, Deref, Clone)]
pub struct XrSession(
    #[deref] pub(crate) openxr::Session<AnyGraphics>,
    pub(crate) GraphicsWrap<Self>,
);

impl GraphicsType for XrSession {
    type Inner<G: GraphicsExt> = openxr::Session<G>;
}

impl<G: GraphicsExt> From<openxr::Session<G>> for XrSession {
    fn from(value: openxr::Session<G>) -> Self {
        Self(value.clone().into_any_graphics(), G::wrap(value))
    }
}

impl XrSession {
    pub fn enumerate_swapchain_formats(&self) -> Result<Vec<wgpu::TextureFormat>> {
        graphics_match!(
            &self.1;
            session => Ok(session.enumerate_swapchain_formats()?.into_iter().filter_map(Api::to_wgpu_format).collect())
        )
    }

    pub fn create_swapchain(&self, info: SwapchainCreateInfo) -> Result<XrSwapchain> {
        Ok(XrSwapchain(graphics_match!(
            &self.1;
            session => Arc::new(Mutex::new(session.create_swapchain(&info.try_into()?)?)) => XrSwapchain
        )))
    }
}

#[derive(Resource, Clone)]
pub struct XrFrameStream(pub(crate) GraphicsWrap<Self>);

impl GraphicsType for XrFrameStream {
    type Inner<G: GraphicsExt> = Arc<Mutex<openxr::FrameStream<G>>>;
}

impl XrFrameStream {
    pub fn begin(&self) -> openxr::Result<()> {
        graphics_match!(
            &self.0;
            stream => stream.lock().unwrap().begin()
        )
    }

    pub fn end(
        &self,
        display_time: openxr::Time,
        environment_blend_mode: openxr::EnvironmentBlendMode,
        layers: &[&dyn CompositionLayer],
    ) -> Result<()> {
        graphics_match!(
            &self.0;
            stream => {
                let mut stream = stream.lock().unwrap();
                let mut new_layers = vec![];

                for (i, layer) in layers.into_iter().enumerate() {
                    if let Some(swapchain) = layer.swapchain() {
                        if !swapchain.0.using_graphics::<Api>() {
                            error!(
                                "Composition layer {i} is using graphics api '{}', expected graphics api '{}'. Excluding layer from frame submission.",
                                swapchain.0.graphics_name(),
                                std::any::type_name::<Api>(),
                            );
                            continue;
                        }
                    }
                    new_layers.push(unsafe { std::mem::transmute(layer.header()) });
                }

                Ok(stream.end(display_time, environment_blend_mode, new_layers.as_slice())?)
            }
        )
    }
}

#[derive(Resource, Deref, DerefMut)]
pub struct XrFrameWaiter(pub openxr::FrameWaiter);

#[derive(Resource, Clone)]
pub struct XrSwapchain(pub(crate) GraphicsWrap<Self>);

impl GraphicsType for XrSwapchain {
    type Inner<G: GraphicsExt> = Arc<Mutex<openxr::Swapchain<G>>>;
}

impl XrSwapchain {
    pub fn acquire_image(&self) -> Result<u32> {
        graphics_match!(
            &self.0;
            swap => Ok(swap.lock().unwrap().acquire_image()?)
        )
    }

    pub fn wait_image(&self, timeout: openxr::Duration) -> Result<()> {
        graphics_match!(
            &self.0;
            swap => Ok(swap.lock().unwrap().wait_image(timeout)?)
        )
    }

    pub fn release_image(&self) -> Result<()> {
        graphics_match!(
            &self.0;
            swap => Ok(swap.lock().unwrap().release_image()?)
        )
    }

    pub fn enumerate_images(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        resolution: UVec2,
    ) -> Result<XrSwapchainImages> {
        graphics_match!(
            &self.0;
            swap => {
                let swap = swap.lock().unwrap();
                let mut images = vec![];
                for image in swap.enumerate_images()? {
                    unsafe {
                        images.push(Api::to_wgpu_img(image, device, format, resolution)?);
                    }
                }
                Ok(XrSwapchainImages(images.into()))
            }
        )
    }
}

#[derive(Deref, Clone, Resource)]
pub struct XrStage(pub Arc<openxr::Space>);

#[derive(Debug, Deref, Resource, Clone)]
pub struct XrSwapchainImages(pub Arc<Vec<wgpu::Texture>>);

#[derive(Copy, Clone, Eq, PartialEq, Deref, DerefMut, Resource, ExtractResource)]
pub struct XrTime(pub openxr::Time);

#[derive(Copy, Clone, Eq, PartialEq, Resource)]
pub struct XrSwapchainInfo {
    pub format: wgpu::TextureFormat,
    pub resolution: UVec2,
}

#[derive(Debug, Copy, Clone, Deref, Default, Eq, PartialEq, Ord, PartialOrd, Hash, Resource)]
pub struct XrSystemId(pub openxr::SystemId);

#[derive(Clone, Copy, Resource)]
pub struct XrGraphicsInfo {
    pub blend_mode: EnvironmentBlendMode,
    pub resolution: UVec2,
    pub format: wgpu::TextureFormat,
}

#[derive(Clone, Resource, ExtractResource, Deref, DerefMut)]
pub struct XrViews(pub Vec<openxr::View>);

#[derive(Clone)]
/// This is used to store information from startup that is needed to create the session after the instance has been created.
pub struct XrSessionCreateInfo {
    /// List of blend modes the openxr session can use. If [None], pick the first available blend mode.
    pub blend_modes: Option<Vec<EnvironmentBlendMode>>,
    /// List of formats the openxr session can use. If [None], pick the first available format
    pub formats: Option<Vec<wgpu::TextureFormat>>,
    /// List of resolutions that the openxr swapchain can use. If [None] pick the first available resolution.
    pub resolutions: Option<Vec<UVec2>>,
    /// Graphics info used to create a session.
    pub graphics_info: XrSessionGraphicsInfo,
}

#[derive(Resource, Clone, Default)]
pub struct XrSessionStarted(Arc<AtomicBool>);

impl XrSessionStarted {
    pub fn set(&self, val: bool) {
        self.0.store(val, Ordering::SeqCst);
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(ExtractResource, Resource, Clone, Copy, Default)]
pub struct XrRootTransform(pub GlobalTransform);
