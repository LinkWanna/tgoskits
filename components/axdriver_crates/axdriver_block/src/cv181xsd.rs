//! SD/MMC driver based on SDIO.

use ax_driver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use log::debug;
use sg200x_bsp::sdmmc::{self, BLOCK_SIZE, CmdError, Sdmmc};

use crate::BlockDriverOps;

/// A SD/MMC driver.
pub struct Cv181xSD(Sdmmc);

impl Cv181xSD {
    /// Creates a new [`SdMmcDriver`] from the given base address.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `base` is a valid pointer to the SD/MMC controller's
    /// register block and that no other code is concurrently accessing the same hardware.
    pub unsafe fn new(sd_base: usize, top_base: usize) -> Self {
        let sdmmc = unsafe { Sdmmc::from_base_addresses(sd_base, top_base) };
        sdmmc
            .init()
            .expect("Failed to initialize SD/MMC controller");
        Self(sdmmc)
    }
}

impl BaseDriverOps for Cv181xSD {
    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }

    fn device_name(&self) -> &str {
        "cv181xsd"
    }
}

impl BlockDriverOps for Cv181xSD {
    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> DevResult {
        if buf.len() < BLOCK_SIZE {
            return Err(DevError::InvalidParam);
        }
        self.0.clk_en(true);
        self.0.read_block(block_id as u32, buf).unwrap();
        self.0.clk_en(false);
        Ok(())
    }

    fn write_block(&mut self, block_id: u64, buf: &[u8]) -> DevResult {
        if buf.len() < BLOCK_SIZE {
            return Err(DevError::Io);
        }

        // sg200x_bsp 的代码可能有问题（没有批量写），但是我尽量不改，先这样写吧
        self.0.clk_en(true);
        assert!(buf.len() % BLOCK_SIZE == 0);
        for chunk in buf.chunks_exact(BLOCK_SIZE) {
            self.0.write_block(block_id as u32, chunk).unwrap();
        }
        self.0.clk_en(false);

        Ok(())
    }

    fn flush(&mut self) -> DevResult {
        Ok(())
    }

    fn num_blocks(&self) -> u64 {
        self.0.card_capacity_blocks()
    }

    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }
}
