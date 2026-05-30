//! USB 标准描述符类型，对齐 USB 2.0 规范。
//!
//! - DeviceDescriptor:      §9.6.1 (18 bytes)
//! - ConfigurationDescriptor: §9.6.3
//! - InterfaceDescriptor:   §9.6.5
//! - EndpointDescriptor:    §9.6.6

use alloc::vec::Vec;

/// USB 设备描述符，对齐 USB 2.0 §9.6.1。
#[derive(Debug, Clone)]
pub struct DeviceDescriptor {
    /// bcdUSB — USB 规范版本号（如 0x0200 = USB 2.0）
    pub usb_version: u16,
    /// bDeviceClass
    pub class: u8,
    /// bDeviceSubClass
    pub subclass: u8,
    /// bDeviceProtocol
    pub protocol: u8,
    /// bMaxPacketSize0 — EP0 最大包大小（仅 8/16/32/64 合法）
    pub max_packet_size_0: u8,
    /// idVendor
    pub vendor_id: u16,
    /// idProduct
    pub product_id: u16,
    /// bcdDevice
    pub device_version: u16,
    /// iManufacturer — 厂商字符串索引（0 = 无）
    pub manufacturer_str_idx: u8,
    /// iProduct — 产品字符串索引（0 = 无）
    pub product_str_idx: u8,
    /// iSerialNumber — 序列号字符串索引（0 = 无）
    pub serial_str_idx: u8,
    /// bNumConfigurations
    pub num_configurations: u8,
}

/// USB 配置描述符，对齐 USB 2.0 §9.6.3。
#[derive(Debug, Clone)]
pub struct ConfigurationDescriptor {
    /// wTotalLength — 该配置所有描述符的总长度
    pub total_length: u16,
    /// bNumInterfaces
    pub num_interfaces: u8,
    /// bConfigurationValue — SET_CONFIGURATION 时使用的值
    pub configuration_value: u8,
    /// iConfiguration — 配置字符串索引（0 = 无）
    pub config_str_idx: u8,
    /// bmAttributes — bit 6=自供电, bit 5=远程唤醒, bit 7=必须为1
    pub attributes: u8,
    /// bMaxPower — 最大功耗，单位 2mA
    pub max_power: u8,
    /// 该配置下的接口列表
    pub interfaces: Vec<InterfaceDescriptor>,
}

/// USB 接口描述符，对齐 USB 2.0 §9.6.5。
#[derive(Debug, Clone)]
pub struct InterfaceDescriptor {
    /// bInterfaceNumber
    pub interface_number: u8,
    /// bAlternateSetting
    pub alternate_setting: u8,
    /// bNumEndpoints — 该接口使用的端点数量（不含 EP0）
    pub num_endpoints: u8,
    /// bInterfaceClass
    pub class: u8,
    /// bInterfaceSubClass
    pub subclass: u8,
    /// bInterfaceProtocol
    pub protocol: u8,
    /// iInterface — 接口字符串索引（0 = 无）
    pub interface_str_idx: u8,
    /// 该接口下的端点列表
    pub endpoints: Vec<EndpointDescriptor>,
}

/// USB 端点描述符，对齐 USB 2.0 §9.6.6。
#[derive(Debug, Clone)]
pub struct EndpointDescriptor {
    /// bEndpointAddress — bit 7: 方向(0=OUT,1=IN), bit 0-3: 端点号
    pub address: u8,
    /// bmAttributes — bit 0-1: 传输类型(0=Control,1=Isoch,2=Bulk,3=Interrupt)
    pub attributes: u8,
    /// wMaxPacketSize — 最大包大小
    pub max_packet_size: u16,
    /// bInterval — 轮询间隔（帧/微帧）
    pub interval: u8,
}

impl EndpointDescriptor {
    /// 端点方向：bit 7 为 1 表示 IN（设备→主机）
    #[inline]
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    /// 端点方向：bit 7 为 0 表示 OUT（主机→设备）
    #[inline]
    pub fn is_out(&self) -> bool {
        self.address & 0x80 == 0
    }

    /// 端点号（低 4 位）
    #[inline]
    pub fn endpoint_number(&self) -> u8 {
        self.address & 0x0F
    }

    /// 传输类型（低 2 位）：0=Control, 1=Isoch, 2=Bulk, 3=Interrupt
    #[inline]
    pub fn transfer_type(&self) -> u8 {
        self.attributes & 0x03
    }
}
