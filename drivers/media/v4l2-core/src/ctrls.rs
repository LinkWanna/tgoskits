//! V4L2 控件处理器框架。
//!
//! 对应 Linux 的 `v4l2-ctrls-core.c`。驱动通过
//! [`CtrlHandler::new_ctrl`] 注册控件，处理器自动将
//! `QUERYCTRL` / `G_CTRL` / `S_CTRL` / `QUERYMENU` 路由到正确的控件。
//!
//! 所有控件值都以 `AtomicI64` 存储，以实现无锁读取。

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicI64, Ordering};

use crate::{
    V4l2Error,
    interface::ctrl::{Control, QueryCtrl, QueryExtCtrl, Querymenu},
};

type Result<T> = core::result::Result<T, V4l2Error>;

// ── 控件类型 ─────────────────────────────────────────────────────────

/// V4L2 控件类型——对应 Linux 枚举 v4l2_ctrl_type。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlType {
    Integer   = 1,
    Boolean   = 2,
    Menu      = 3,
    Button    = 4,
    Integer64 = 5,
}

impl CtrlType {
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => Self::Integer,
            2 => Self::Boolean,
            3 => Self::Menu,
            4 => Self::Button,
            5 => Self::Integer64,
            _ => return None,
        })
    }
}

// ── 菜单项回调 ───────────────────────────────────────────────────

/// 按索引解析菜单控件条目名的回调。
pub type MenuNameFn = Box<dyn Fn(u32) -> Option<&'static str> + Send + Sync>;

/// 硬件控件的值读写回调（对应 Linux `struct v4l2_ctrl_ops`）。
///
/// 注册时以 [`Option<CtrlOps>`] 传入：`Some` = 硬件控件（G/S_CTRL 走设备
/// 回调），`None` = 纯内存控件（读/写内存 cur_val）——对应 Linux 中
/// `v4l2_ctrl_ops` 指针为 NULL 与否，不存在半开状态。
pub struct CtrlOps {
    /// 写回调：把值写入设备（Linux `s_ctrl`），返回实际生效值（可能 clamp）。
    pub set: CtrlSetFn,
    /// 读回调：从设备读取当前值（Linux `g_volatile_ctrl`）。
    pub get: CtrlGetFn,
}

/// 硬件控件值读取回调：返回设备当前值（例如 UVC 走 USB GET_CUR）。
pub type CtrlGetFn = Box<dyn Fn() -> Result<i64> + Send + Sync>;

/// 硬件控件值设置回调：写入设备并返回实际生效值（设备可能 clamp）。
/// 例如 UVC 走 USB SET_CUR 后按 GET_CUR 读回实际值。
pub type CtrlSetFn = Box<dyn Fn(i64) -> Result<i64> + Send + Sync>;

// ── 控件引用 ────────────────────────────────────────────────────

/// 已注册的控件，包含元数据和当前值。
pub struct CtrlRef {
    /// V4L2 控件 ID（CID）。
    pub id: u32,
    /// 人类可读的名称。
    pub name: &'static str,
    /// 控件类型（整数、布尔、菜单等）。
    pub ctrl_type: CtrlType,
    /// 允许的最小值。
    pub minimum: i64,
    /// 允许的最大值。
    pub maximum: i64,
    /// 步长（用于整数控件）。
    pub step: i64,
    /// 默认值。
    pub default_value: i64,
    /// 标志位（只读、volatile 等）。
    pub flags: u32,
    /// 当前值（通过 AtomicI64 无锁访问）。
    pub cur_val: AtomicI64,
    /// 菜单控件专用：将索引解析为名称。
    menu_fn: Option<MenuNameFn>,
    /// 硬件代理回调；`None` = 纯内存控件。
    ops: Option<CtrlOps>,
}

impl CtrlRef {
    fn new_int(
        id: u32,
        name: &'static str,
        min: i64,
        max: i64,
        step: i64,
        default: i64,
        ops: Option<CtrlOps>,
    ) -> Self {
        Self {
            id,
            name,
            ctrl_type: CtrlType::Integer,
            minimum: min,
            maximum: max,
            step,
            default_value: default,
            flags: 0,
            cur_val: AtomicI64::new(default),
            menu_fn: None,
            ops,
        }
    }

    fn new_bool(id: u32, name: &'static str, default: bool, ops: Option<CtrlOps>) -> Self {
        Self {
            id,
            name,
            ctrl_type: CtrlType::Boolean,
            minimum: 0,
            maximum: 1,
            step: 1,
            default_value: default as i64,
            flags: 0,
            cur_val: AtomicI64::new(default as i64),
            menu_fn: None,
            ops,
        }
    }

    fn new_menu(
        id: u32,
        name: &'static str,
        max: i64,
        default: i64,
        menu_fn: MenuNameFn,
        ops: Option<CtrlOps>,
    ) -> Self {
        Self {
            id,
            name,
            ctrl_type: CtrlType::Menu,
            minimum: 0,
            maximum: max,
            step: 1,
            default_value: default,
            flags: 0,
            cur_val: AtomicI64::new(default),
            menu_fn: Some(menu_fn),
            ops,
        }
    }

    /// 获取当前值。
    pub fn value(&self) -> i64 {
        self.cur_val.load(Ordering::Acquire)
    }

    /// 设置当前值（不校验——调用方必须自行校验）。
    pub fn set_value(&self, v: i64) {
        self.cur_val.store(v, Ordering::Release);
    }
}

// ── 控件处理器 ──────────────────────────────────────────────────────

/// 管理一组 V4L2 控件。
///
/// 用法：
/// ```ignore
/// let mut handler = CtrlHandler::new();
/// handler.new_int(BRIGHTNESS_CID, "Brightness", 0, 255, 1, 128);
/// handler.new_bool(HFLIP_CID, "Horizontal Flip", false);
/// ```
///
/// 然后通过以下方式路由 ioctl：
/// - 用 `handler.queryctrl(&mut q)?` 处理 `VIDIOC_QUERYCTRL`
/// - 用 `handler.g_ctrl(&mut c)?` 处理 `VIDIOC_G_CTRL`
/// - 用 `handler.s_ctrl(&c)` → `Option<u32>` 处理 `VIDIOC_S_CTRL`
///   （返回发生变化的控件 ID 用于事件广播）
pub struct CtrlHandler {
    ctrls: Vec<CtrlRef>,
}

impl CtrlHandler {
    /// 创建空处理器。
    pub fn new() -> Self {
        Self { ctrls: Vec::new() }
    }

    // ── 注册 ─────────────────────────────────────────────────

    /// 按 id 升序插入（对齐 Linux v4l2_ctrl_handler 的有序存储）。
    ///
    /// NEXT_CTRL 枚举依赖有序表：严格 `>` 查找在乱序表上会漏掉
    /// id 小于首个控件的项。
    fn insert_sorted(&mut self, ctrl: CtrlRef) {
        let pos = self
            .ctrls
            .iter()
            .position(|c| c.id > ctrl.id)
            .unwrap_or(self.ctrls.len());
        self.ctrls.insert(pos, ctrl);
    }

    /// 注册一个整数控件。
    ///
    /// `ops`：`Some` = 硬件控件（读写走设备回调）；`None` = 纯内存控件。
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
    ) {
        self.insert_sorted(CtrlRef::new_int(id, name, min, max, step, default, ops));
    }

    /// 注册一个布尔控件。
    pub fn new_bool(&mut self, id: u32, name: &'static str, default: bool, ops: Option<CtrlOps>) {
        self.insert_sorted(CtrlRef::new_bool(id, name, default, ops));
    }

    /// 注册带名称解析回调的菜单控件。
    pub fn new_menu(
        &mut self,
        id: u32,
        name: &'static str,
        items: u32,
        default: u32,
        menu_fn: impl Fn(u32) -> Option<&'static str> + Send + Sync + 'static,
        ops: Option<CtrlOps>,
    ) {
        self.insert_sorted(CtrlRef::new_menu(
            id,
            name,
            items as i64 - 1,
            default as i64,
            Box::new(menu_fn),
            ops,
        ));
    }

    // ── 查找 ───────────────────────────────────────────────────────

    /// 按 ID 查找控件。
    pub fn find(&self, id: u32) -> Option<&CtrlRef> {
        self.ctrls.iter().find(|c| c.id == id)
    }

    /// 按 ID 查找控件（可变——仅供内部使用）。
    #[allow(dead_code)]
    fn find_mut(&mut self, id: u32) -> Option<&mut CtrlRef> {
        self.ctrls.iter_mut().find(|c| c.id == id)
    }

    /// 已注册控件的数量。
    pub fn count(&self) -> usize {
        self.ctrls.len()
    }

    /// 遍历所有控件的迭代器。
    pub fn iter(&self) -> impl Iterator<Item = &CtrlRef> {
        self.ctrls.iter()
    }
}

/// IOCTL 路由——由驱动委托的方法（`IoctlOps::queryctrl`/`g_ctrl`/`s_ctrl` 等）。
impl CtrlHandler {
    // ── IOCTL 路由 ────────────────────────────────────────────────

    /// 处理 `VIDIOC_QUERYCTRL`：为给定的控件 ID 填充 queryctrl。
    ///
    /// 支持 `V4L2_CTRL_FLAG_NEXT_CTRL` 用于枚举。
    pub fn queryctrl(&self, q: &mut QueryCtrl) -> Result<()> {
        let want_next = (q.id & 0x8000_0000) != 0;
        let search_id = q.id & !0x8000_0000;

        if want_next {
            let mut found = false;
            for c in &self.ctrls {
                if !found && c.id > search_id {
                    found = true;
                    Self::fill_queryctrl(q, c);
                    break;
                }
            }
            if !found {
                return Err(V4l2Error::InvalidArgument);
            }
        } else {
            let c = self.find(search_id).ok_or(V4l2Error::InvalidArgument)?;
            Self::fill_queryctrl(q, c);
        }
        Ok(())
    }

    fn fill_queryctrl(q: &mut QueryCtrl, c: &CtrlRef) {
        q.id = c.id;
        q.ty = c.ctrl_type as u32;
        let name = c.name.as_bytes();
        let len = name.len().min(31);
        q.name[..len].copy_from_slice(&name[..len]);
        q.minimum = c.minimum as i32;
        q.maximum = c.maximum as i32;
        q.step = c.step as i32;
        q.default_value = c.default_value as i32;
        q.flags = crate::interface::ctrl::CtrlFlags::empty();
    }

    /// 处理 `VIDIOC_G_CTRL`：读取当前值。
    ///
    /// 控件注册了 `ops.get`（Linux `g_volatile_ctrl`）则从设备读取；
    /// 否则读内存 cur_val。
    pub fn g_ctrl(&self, ctrl: &mut Control) -> Result<()> {
        let c = self.find(ctrl.id).ok_or(V4l2Error::InvalidArgument)?;
        ctrl.value = if let Some(ops) = &c.ops {
            (ops.get)()? as i32
        } else {
            c.value() as i32
        };
        Ok(())
    }

    /// 处理 `VIDIOC_S_CTRL`：设置值（按 min/max 校验）。
    ///
    /// 控件注册了 `ops.set`（Linux `s_ctrl`）则写入设备并按返回的实际值更新；
    /// 否则写内存 cur_val。
    /// 返回发生变化的控件的 `Some(id)` 用于事件广播，值未变化则返回 `None`。
    pub fn s_ctrl(&self, ctrl: &Control) -> Result<Option<u32>> {
        let c = self.find(ctrl.id).ok_or(V4l2Error::InvalidArgument)?;
        let v = ctrl.value as i64;
        if v < c.minimum || v > c.maximum {
            return Err(V4l2Error::InvalidArgument);
        }
        let old = c.value();
        let new = if let Some(ops) = &c.ops {
            (ops.set)(v)? // 设备实际生效值（可能 clamp）
        } else {
            v
        };
        if old == new {
            return Ok(None); // 值未变化，不发事件
        }
        c.set_value(new);
        Ok(Some(c.id))
    }

    /// 处理 `VIDIOC_QUERYMENU`：按索引解析菜单条目名。
    pub fn querymenu(&self, q: &mut Querymenu) -> Result<()> {
        let c = self.find(q.id).ok_or(V4l2Error::InvalidArgument)?;
        let Some(ref menu_fn) = c.menu_fn else {
            return Err(V4l2Error::InvalidArgument);
        };
        let name = menu_fn(q.index).ok_or(V4l2Error::InvalidArgument)?;
        let b = name.as_bytes();
        let len = b.len().min(31);
        q.name[..len].copy_from_slice(&b[..len]);
        Ok(())
    }

    /// 处理 `VIDIOC_QUERY_EXT_CTRL`。
    pub fn query_ext_ctrl(&self, q: &mut QueryExtCtrl) -> Result<()> {
        let c = self.find(q.id).ok_or(V4l2Error::InvalidArgument)?;
        q.id = c.id;
        q.ty = c.ctrl_type as u32;
        let name = c.name.as_bytes();
        let len = name.len().min(31);
        q.name[..len].copy_from_slice(&name[..len]);
        q.minimum = c.minimum;
        q.maximum = c.maximum;
        q.step = c.step as u64;
        q.default_value = c.default_value;
        q.flags = crate::interface::ctrl::CtrlFlags::empty();
        Ok(())
    }
}

impl Default for CtrlHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::ctrl::QueryCtrl;

    const NEXT_CTRL: u32 = 0x8000_0000;

    fn zero_query_ctrl() -> QueryCtrl {
        // QueryCtrl 无 Default（repr(C) 结构），手动清零。
        QueryCtrl {
            id: 0,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: crate::interface::ctrl::CtrlFlags::empty(),
            reserved: [0; 2],
        }
    }

    fn register_uvc_like(handler: &mut CtrlHandler) {
        // 乱序注册（对齐 UVC_CONTROL_DEFS 顺序：非 id 升序）。
        handler.new_int(0x0098_091C, "Backlight", 0, 255, 1, 0, None);
        handler.new_int(0x0098_0900, "Brightness", 0, 255, 1, 0, None);
        handler.new_int(0x0098_0901, "Contrast", 0, 255, 1, 0, None);
        handler.new_int(0x009A_0901, "ExposureAuto", 0, 4, 1, 0, None);
        handler.new_int(0x009A_0902, "ExposureAbs", 0, 10_000, 1, 0, None);
    }

    /// 回归：NEXT_CTRL 枚举必须严格递增、返回 id 不得携带 NEXT 标志、
    /// 枚举完必须 EINVAL 终止。
    ///
    /// 原 bug：`c.id == search_id` 置 found 但不填充，返回 Ok 且 q.id 保持
    /// 输入值（带 NEXT 标志）→ 用户态拿带标志的 id 继续枚举 → 永远命中
    /// 同一控件 → 无限成功（compliance storeState 卡死 / repro QC 循环）。
    #[test]
    fn next_ctrl_enumeration_is_strictly_increasing_and_terminates() {
        let mut handler = CtrlHandler::new();
        register_uvc_like(&mut handler);

        let mut q = zero_query_ctrl();
        q.id = NEXT_CTRL; // 从 0 之后开始
        let mut last_id = 0u32;
        let mut count = 0u32;
        loop {
            match handler.queryctrl(&mut q) {
                Ok(()) => {
                    // 关键断言 1：返回 id 不得携带 NEXT 标志（bug 时返回 0x8098xxxx）
                    assert_eq!(
                        q.id & NEXT_CTRL,
                        0,
                        "returned id {:#x} carries NEXT flag (bug: id not refilled)",
                        q.id
                    );
                    // 关键断言 2：严格递增（不允许回绕/重复）
                    assert!(
                        q.id > last_id,
                        "id not strictly increasing: {:#x} <= {:#x}",
                        q.id,
                        last_id
                    );
                    last_id = q.id;
                    count += 1;
                    // 模拟用户态推进（Linux v4l_queryctrl：返回 id + NEXT 标志）
                    q.id = q.id | NEXT_CTRL;
                    assert!(count <= 16, "enumeration did not terminate");
                }
                Err(V4l2Error::InvalidArgument) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(
            count, 5,
            "should enumerate exactly the 5 registered controls"
        );
    }

    /// NEXT_CTRL 从 0 开始返回第一个控件；超过最大 id 必须 EINVAL。
    #[test]
    fn next_ctrl_first_and_beyond_end() {
        let mut handler = CtrlHandler::new();
        register_uvc_like(&mut handler);

        let mut q = zero_query_ctrl();
        q.id = NEXT_CTRL;
        handler.queryctrl(&mut q).unwrap();
        assert_eq!(
            q.id, 0x0098_0900,
            "first control should be smallest id (Brightness)"
        );

        // 超过所有控件：EINVAL
        q.id = 0x009A_0902 | NEXT_CTRL; // 最大 id 之后
        assert!(matches!(
            handler.queryctrl(&mut q),
            Err(V4l2Error::InvalidArgument)
        ));
    }
}
