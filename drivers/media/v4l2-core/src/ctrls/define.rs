use core::sync::atomic::AtomicI64;

use crate::{
    Result, V4l2Error,
    ctrls::{Ctrl, CtrlConfig, CtrlHandler, CtrlOps, CtrlType},
    interface::ctrl::{CID_PRIVATE_BASE, CtrlFlags},
};

impl CtrlHandler {
    /// 按 id 有序插入（对齐 Linux 的 `ctrl_refs` 有序表）；重复 id 拒绝。
    fn insert_sorted(&mut self, ctrl: Ctrl) -> Result<()> {
        let id = ctrl.id;
        match self.ctrls.binary_search_by_key(&id, |c| c.id) {
            Ok(_) => Err(V4l2Error::InvalidArgument),
            Err(pos) => {
                self.ctrls.insert(pos, ctrl);
                Ok(())
            }
        }
    }

    /// 注册一个完整配置的控件（crate 内低层入口；对外请用 `new_int`/`new_bool`/`new_menu` 等类型化接口）。
    pub fn new_ctrl(&mut self, cfg: CtrlConfig) -> Result<()> {
        if cfg.id == 0 || cfg.name.is_empty() || cfg.id >= CID_PRIVATE_BASE {
            return Err(V4l2Error::OutOfRange);
        }
        if cfg.ctrl_type == CtrlType::Menu && cfg.qmenu.is_none() {
            return Err(V4l2Error::OutOfRange);
        }
        cfg.ctrl_type
            .check_range(cfg.minimum, cfg.maximum, cfg.step, cfg.default_value)?;
        if cfg.ctrl_type == CtrlType::Menu
            && let Some(qmenu) = cfg.qmenu
            && cfg.maximum >= 0
            && cfg.maximum as usize >= qmenu.len()
        {
            return Err(V4l2Error::OutOfRange);
        }

        // Linux v4l2_ctrl_new：非 Button / CtrlClass 类型声明 HAS_WHICH_MIN_MAX；
        // Button 强制 WRITE_ONLY | EXECUTE_ON_WRITE；CtrlClass 强制 READ_ONLY。
        let mut flags = cfg.flags;
        if !matches!(cfg.ctrl_type, CtrlType::Button | CtrlType::CtrlClass) {
            flags |= CtrlFlags::HAS_WHICH_MIN_MAX;
        }
        match cfg.ctrl_type {
            CtrlType::Button => {
                flags |= CtrlFlags::WRITE_ONLY | CtrlFlags::EXECUTE_ON_WRITE;
            }
            CtrlType::CtrlClass => flags |= CtrlFlags::READ_ONLY,
            _ => {}
        }

        let ctrl = Ctrl {
            id: cfg.id,
            name: cfg.name,
            ctrl_type: cfg.ctrl_type,
            minimum: cfg.minimum,
            maximum: cfg.maximum,
            step: cfg.step,
            default_value: cfg.default_value,
            flags,
            qmenu: cfg.qmenu,
            ops: cfg.ops,
            cur: AtomicI64::new(cfg.default_value),
        };
        self.insert_sorted(ctrl)
    }

    /// 注册一个整数控件（硬件代理时 `ops.is_some()` 自动追加 `VOLATILE`）。
    #[allow(clippy::too_many_arguments)]
    pub fn new_int(
        &mut self,
        id: u32,
        name: &'static str,
        min: i64,
        max: i64,
        step: i64,
        default: i64,
        ops: Option<CtrlOps>,
    ) -> Result<()> {
        let flags = if ops.is_some() {
            CtrlFlags::VOLATILE
        } else {
            CtrlFlags::empty()
        };
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Integer,
            minimum: min,
            maximum: max,
            step: step as u64,
            default_value: default,
            flags,
            qmenu: None,
            ops,
        })
    }

    /// 注册一个布尔控件（硬件代理时自动 `VOLATILE`）。
    pub fn new_bool(
        &mut self,
        id: u32,
        name: &'static str,
        default: bool,
        ops: Option<CtrlOps>,
    ) -> Result<()> {
        let flags = if ops.is_some() {
            CtrlFlags::VOLATILE
        } else {
            CtrlFlags::empty()
        };
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Boolean,
            minimum: 0,
            maximum: 1,
            step: 1,
            default_value: default as i64,
            flags,
            qmenu: None,
            ops,
        })
    }

    /// 注册带静态菜单项数组的菜单控件（Linux `v4l2_ctrl_new_std_menu_items`）。
    pub fn new_menu(
        &mut self,
        id: u32,
        name: &'static str,
        items: u32,
        default: u32,
        qmenu: &'static [&'static str],
        ops: Option<CtrlOps>,
    ) -> Result<()> {
        let flags = if ops.is_some() {
            CtrlFlags::VOLATILE
        } else {
            CtrlFlags::empty()
        };
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Menu,
            minimum: 0,
            maximum: items as i64 - 1,
            step: 0,
            default_value: default as i64,
            flags,
            qmenu: Some(qmenu),
            ops,
        })
    }

    /// 注册一个按钮控件（`Button` 恒为 `WRITE_ONLY|EXECUTE_ON_WRITE`）。
    pub fn new_button(&mut self, id: u32, name: &'static str, ops: Option<CtrlOps>) -> Result<()> {
        self.new_ctrl(CtrlConfig {
            id,
            name,
            ctrl_type: CtrlType::Button,
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            qmenu: None,
            ops,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{V4l2Error, ctrls::CtrlType, interface::ctrl::CtrlFlags};

    const BRIGHTNESS: u32 = 0x0098_0900;

    #[test]
    fn new_ctrl_rejects_duplicate_id() {
        let mut h = CtrlHandler::new();
        h.new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 0, None)
            .unwrap();
        let err = h
            .new_int(BRIGHTNESS, "Brightness2", 0, 255, 1, 0, None)
            .unwrap_err();
        assert!(matches!(err, V4l2Error::InvalidArgument));
    }

    #[test]
    fn new_ctrl_rejects_out_of_range() {
        let mut h = CtrlHandler::new();
        // min > max
        assert!(h.new_int(BRIGHTNESS, "Bad", 100, 0, 1, 0, None).is_err());
        // Menu without qmenu
        assert!(
            h.new_ctrl(CtrlConfig {
                id: BRIGHTNESS,
                name: "NoMenu",
                ctrl_type: CtrlType::Menu,
                minimum: 0,
                maximum: 1,
                step: 0,
                default_value: 0,
                flags: CtrlFlags::empty(),
                qmenu: None,
                ops: None,
            })
            .is_err()
        );
        // Button should succeed even with 0 range
        let mut h2 = CtrlHandler::new();
        h2.new_button(0x0098_0931, "Btn", None).unwrap();
        assert_eq!(h2.len(), 1);
    }

    #[test]
    fn new_int_auto_appends_volatile_for_hardware_proxy() {
        let mut h = CtrlHandler::new();
        let ops = crate::ctrls::CtrlOps {
            get: Some(Box::new(|| Ok(0))),
            try_ctrl: None,
            set: Box::new(|v| Ok(v)),
        };
        h.new_int(BRIGHTNESS, "HW", 0, 255, 1, 0, Some(ops))
            .unwrap();
        let ctrl = h.find(BRIGHTNESS).unwrap();
        assert!(ctrl.flags.contains(CtrlFlags::VOLATILE));
        assert!(ctrl.flags.contains(CtrlFlags::HAS_WHICH_MIN_MAX));

        let mut h2 = CtrlHandler::new();
        h2.new_int(BRIGHTNESS, "SW", 0, 255, 1, 0, None).unwrap();
        let ctrl2 = h2.find(BRIGHTNESS).unwrap();
        assert!(!ctrl2.flags.contains(CtrlFlags::VOLATILE));
    }

    #[test]
    fn new_menu_validates_items_and_qmenu_len() {
        let menu = &["A", "B", "C"];
        let mut h = CtrlHandler::new();
        // items 3, default 2 ok
        h.new_menu(0x0098_0900, "Menu", 3, 2, menu, None).unwrap();
        // default out of range
        assert!(h.new_menu(0x0098_0901, "Bad", 3, 5, menu, None).is_err());
        // qmenu len < items
        assert!(h.new_menu(0x0098_0902, "Bad2", 5, 0, menu, None).is_err());
    }

    #[test]
    fn new_button_forces_write_only_execute() {
        let mut h = CtrlHandler::new();
        h.new_button(0x0098_0931, "Btn", None).unwrap();
        let ctrl = h.find(0x0098_0931).unwrap();
        assert!(ctrl.flags.contains(CtrlFlags::WRITE_ONLY));
        assert!(ctrl.flags.contains(CtrlFlags::EXECUTE_ON_WRITE));
        assert!(!ctrl.flags.contains(CtrlFlags::HAS_WHICH_MIN_MAX));
        assert_eq!(ctrl.ctrl_type, CtrlType::Button);
    }
}
