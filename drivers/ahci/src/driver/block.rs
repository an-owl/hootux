use super::Port;
use crate::driver::CmdErr;
use alloc::boxed::Box;
use core::any::Any;
use futures::FutureExt;
use hootux::system::sysfs::block;

#[derive(Clone)]
pub struct AhciBlockDev {
    id: block::BlockDeviceId,
    port: alloc::sync::Weak<Port>,
    geom: alloc::sync::Arc<spin::RwLock<Option<block::BlockDevGeom>>>,
}

impl AhciBlockDev {
    pub(crate) fn new(r#gen: &alloc::sync::Arc<Port>, id: block::BlockDeviceId) -> Self {
        let port = alloc::sync::Arc::downgrade(r#gen);

        Self {
            id,
            port,
            geom: Default::default(),
        }
    }
}

impl block::BlockDev for AhciBlockDev {
    fn read(&self, seek: block::BlockDevGeomIntegral, size: usize) -> block::IoFut<Box<[u8]>> {
        async move {
            // If this returns err then the caller must drop self
            let port = self
                .port
                .upgrade()
                .ok_or(block::BlockDevIoErr::DeviceOffline)?;

            port.get_identity().await;
            let r = port
                .read(
                    super::SectorAddress::new(seek).ok_or(block::BlockDevIoErr::OutOfRange)?,
                    super::SectorCount::new(
                        size.try_into()
                            .map_err(|_| block::BlockDevIoErr::GeomError)?,
                    )
                    .ok_or(block::BlockDevIoErr::GeomError)?,
                )
                .await;
            return if r.is_ok() {
                Ok(r.unwrap())
            } else {
                match r.unwrap_err() {
                    CmdErr::AtaErr => Err(block::BlockDevIoErr::InternalDriverErr),
                    CmdErr::DevErr(e) => {
                        // todo log this earlier when more context is available
                        log::error!("SATA Device returned Error {}", e);
                        Err(block::BlockDevIoErr::HardwareError)
                    }
                    CmdErr::Disowned => Err(block::BlockDevIoErr::DeviceOffline),
                    CmdErr::BadArgs => Err(block::BlockDevIoErr::OutOfRange),
                    CmdErr::BuildErr(_) => Err(block::BlockDevIoErr::InternalDriverErr),
                }
            };
        }
        .boxed()
    }

    fn write(
        &self,
        seek: block::BlockDevGeomIntegral,
        buff: block::IoBuffer,
    ) -> block::IoFut<block::IoBuffer> {
        async move {
            let port = self
                .port
                .upgrade()
                .ok_or(block::BlockDevIoErr::DeviceOffline)?;

            // buffer must be word aligned
            if buff.len() % 2 != 0 || buff.addr() % 2 != 0 {
                return Err(block::BlockDevIoErr::Misaligned);
            }

            let b_len = buff.len();
            if b_len % self.geom().await?.block_size as usize != 0 {
                return Err(block::BlockDevIoErr::GeomError);
            }

            let u = port
                .write(
                    super::SectorAddress::new(seek).ok_or(block::BlockDevIoErr::OutOfRange)?,
                    buff.buff(),
                )
                .await;

            if u.is_err() {
                match u.unwrap_err() {
                    CmdErr::AtaErr => Err(block::BlockDevIoErr::InternalDriverErr),
                    CmdErr::DevErr(e) => {
                        // todo log this earlier when more context is available
                        log::error!("SATA Device returned Error {}", e);
                        Err(block::BlockDevIoErr::HardwareError)
                    }
                    CmdErr::Disowned => Err(block::BlockDevIoErr::DeviceOffline),
                    CmdErr::BadArgs => Err(block::BlockDevIoErr::OutOfRange),
                    CmdErr::BuildErr(_) => Err(block::BlockDevIoErr::InternalDriverErr),
                }
            } else {
                Ok(buff)
            }
        }
        .boxed()
    }

    fn geom(&self) -> block::IoFut<block::BlockDevGeom> {
        async {
            let geom = self.geom.read();
            if self.geom.read().is_some() {
                Ok(geom.unwrap())
            } else {
                // explicitly free mutex
                drop(geom);
                let p = self
                    .port
                    .upgrade()
                    .ok_or(block::BlockDevIoErr::DeviceOffline)?;
                let ident = p.get_identity().await;
                let max_blocks = if ident.support_48_bit {
                    1 << 16
                } else {
                    1 << 8
                };

                Ok(block::BlockDevGeom {
                    blocks: ident.lba_count,
                    block_size: ident.lba_size,
                    optimal_block_size: ident.phys_sec_size,
                    optimal_alignment: ident.offset.into(),
                    max_blocks_per_transfer: max_blocks,
                    req_data_alignment: 2,
                })
            }
        }
        .boxed()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn b_clone(&self) -> Box<dyn block::BlockDev> {
        Box::new(self.clone())
    }
}

impl block::SysFsBlockDevice for AhciBlockDev {
    fn get_id(&self) -> block::BlockDeviceId {
        self.id
    }

    fn s_clone(self: &Self) -> Box<dyn block::SysFsBlockDevice> {
        Box::new(self.clone())
    }
}
