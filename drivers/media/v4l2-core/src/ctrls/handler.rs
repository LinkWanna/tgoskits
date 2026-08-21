//! V4L2 控件处理器——对应 Linux 的 `v4l2-ctrls-core.c` 与 `v4l2-ctrls-api.c`。
//!
//! 驱动通过 [`CtrlHandler`] 注册控件（[`new_ctrl`](CtrlHandler::new_ctrl) 或
//! [`new_int`](CtrlHandler::new_int) / [`new_bool`](CtrlHandler::new_bool) /
//! [`new_menu`](CtrlHandler::new_menu)），处理器统一实现：
//!
//! - 查询：`QUERY_EXT_CTRL` / `QUERYCTRL` / `QUERYMENU`（含 NEXT_CTRL 枚举）；
//! - 主线读写：`G/S/TRY_EXT_CTRLS`（`which`、`error_idx`、校验、取整语义）；
//! - 弃用兼容：`G_CTRL` / `S_CTRL`（Linux `v4l2_g_ctrl` / `v4l2_s_ctrl`）；
//! - 控件事件：订阅（含 `SEND_INITIAL`）与值变化通知。
//!
//! 当前子集（务实范围）：标量类型 Integer / Boolean / Menu / Button /
//! Integer64 / CtrlClass / Bitmask；不实现 cluster、media request、compound /
//! 字符串 / 数组控件。
//!
//! 控件当前值以 [`AtomicI64`] 存储：ioctl 路径在设备锁内写入，驱动的填充
//! 线程（如 vivid 的测试图案填充）可无锁读取，与旧实现保持一致。

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::AtomicI64;

use crate::{
    Result, V4l2Error,
    ctrls::{Ctrl, CtrlConfig, CtrlOps, CtrlType},
    filehandler::{CtrlEventParams, EventOps, V4l2Fh, build_ctrl_event},
    interface::{
        ctrl::{
            CID_PRIVATE_BASE, CTRL_ID_MASK, CTRL_MAX_DIMS, CTRL_WHICH_DEF_VAL, CTRL_WHICH_MAX_VAL,
            CTRL_WHICH_MIN_VAL, CTRL_WHICH_REQUEST_VAL, Control, CtrlFlags, ExtControl,
            ExtControls, QueryCtrl, QueryExtCtrl, Querymenu,
        },
        event::{CtrlChange, Event, EventSubFlags, EventSubscription, EventType},
    },
};

const CTRL_NEXT_CTRL: u32 = 0x8000_0000;
const CTRL_NEXT_COMPOUND: u32 = 0x4000_0000;

/// 值变化通知回调：控件值在 `S_CTRL` / `S_EXT_CTRLS` 中改变时触发，
/// 载荷为完整 `V4L2_EVENT_CTRL` 事件。驱动用它把事件推入共享事件队列。
pub type CtrlChangeNotify = Box<dyn Fn(Event) + Send + Sync>;

// ── 控件处理器 ─────────────────────────────────────────────────────

/// 管理一组 V4L2 控件
///
/// 控件按 id 升序存储（NEXT_CTRL 枚举依赖严格有序表）。ioctl 路径在设备锁
/// 内调用；读取值的方法可无锁并发。
pub struct CtrlHandler {
    ctrls: Vec<Ctrl>,
    /// 值变化通知回调（由驱动在初始化时设置，推送 `V4L2_EVENT_CTRL`）。
    notify: Option<CtrlChangeNotify>,
}

impl CtrlHandler {
    /// 创建空处理器。
    pub fn new() -> Self {
        Self {
            ctrls: Vec::new(),
            notify: None,
        }
    }

    /// 设置值变化通知回调（Linux `v4l2_ctrl_notify` 的事件投递角色）。
    pub fn set_change_notify(&mut self, notify: CtrlChangeNotify) {
        self.notify = Some(notify);
    }

    // ── 注册 ─────────────────────────────────────────────────

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
    pub(crate) fn new_ctrl(&mut self, cfg: CtrlConfig) -> Result<()> {
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

    // ── 查找 ───────────────────────────────────────────────────────

    /// 按 ID 查找控件。
    pub fn find(&self, id: u32) -> Option<&Ctrl> {
        let id = id & CTRL_ID_MASK;
        self.ctrls
            .binary_search_by_key(&id, |c| c.id)
            .ok()
            .map(|i| &self.ctrls[i])
    }

    /// 按 ID 读取当前值。
    pub fn value(&self, id: u32) -> Option<i64> {
        self.find(id).map(Ctrl::value)
    }

    /// 已注册控件的数量。
    pub fn len(&self) -> usize {
        self.ctrls.len()
    }

    /// 是否没有注册任何控件。
    pub fn is_empty(&self) -> bool {
        self.ctrls.is_empty()
    }

    /// 遍历所有控件的迭代器（按 id 升序；同时实现 `ExactSizeIterator` /
    /// `DoubleEndedIterator`）。
    pub fn iter(&self) -> core::slice::Iter<'_, Ctrl> {
        self.ctrls.iter()
    }

    /// `NEXT_CTRL` 枚举：返回 id 严格大于 `id` 的下一个控件；到表尾返回 `None`。
    ///
    /// 控件表按 id 升序，`partition_point` 直接定位首个 `id > id` 的位置。
    /// 当前子集无 compound / 数组控件：`NEXT_COMPOUND` 单独使用不匹配任何
    /// 控件（Linux 中它匹配"隐藏的 compound 控件"）。
    fn next_ctrl(&self, id: u32, next_compound_only: bool, next_all: bool) -> Option<&Ctrl> {
        let pos = self.ctrls.partition_point(|c| c.id <= id);
        self.ctrls[pos..]
            .iter()
            .find(|_| next_ctrl_match(next_compound_only, next_all))
    }

    // ── 查询 IOCTL ────────────────────────────────────────────────

    /// 处理 `VIDIOC_QUERY_EXT_CTRL`（对应 Linux `v4l2_query_ext_ctrl`）。
    ///
    /// 支持 `NEXT_CTRL` / `NEXT_COMPOUND` 标志用于枚举；枚举到末尾返回
    /// `InvalidArgument`（EINVAL）终止。
    pub fn query_ext_ctrl(&self, q: &mut QueryExtCtrl) -> Result<()> {
        let next_flags = CTRL_NEXT_CTRL | CTRL_NEXT_COMPOUND;
        let id = q.id & CTRL_ID_MASK;
        let enum_next = (q.id & next_flags) != 0 && !self.ctrls.is_empty();
        let next_compound_only = (q.id & next_flags) == CTRL_NEXT_COMPOUND;
        let next_all = (q.id & next_flags) == next_flags;

        let ctrl = if enum_next {
            self.next_ctrl(id, next_compound_only, next_all)
        } else {
            self.find(id)
        }
        .ok_or(V4l2Error::InvalidArgument)?;

        q.id = if id >= CID_PRIVATE_BASE { id } else { ctrl.id };
        q.ty = ctrl.ctrl_type as u32;
        let name = ctrl.name.as_bytes();
        let len = name.len().min(q.name.len() - 1);
        q.name[..len].copy_from_slice(&name[..len]);
        q.name[len] = 0;
        q.flags = ctrl.flags;
        q.elem_size = ctrl.ctrl_type.size();
        q.elems = 1;
        q.nr_of_dims = 0;
        q.dims = [0; CTRL_MAX_DIMS as usize];
        q.minimum = ctrl.minimum;
        q.maximum = ctrl.maximum;
        q.default_value = ctrl.default_value;
        q.step = if ctrl.ctrl_type == CtrlType::Menu {
            1
        } else {
            ctrl.step
        };
        q.reserved = [0; 32];
        Ok(())
    }

    /// 处理 `VIDIOC_QUERYCTRL`（对应 Linux `v4l2_queryctrl`）——
    /// 基于 `QUERY_EXT_CTRL` 结果换算。
    pub fn queryctrl(&self, q: &mut QueryCtrl) -> Result<()> {
        let mut qec = QueryExtCtrl {
            id: q.id,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            elem_size: 0,
            elems: 0,
            nr_of_dims: 0,
            dims: [0; CTRL_MAX_DIMS as usize],
            reserved: [0; 32],
        };
        self.query_ext_ctrl(&mut qec)?;

        // v4l2_query_ext_ctrl_to_v4l2_queryctrl：仅标量兼容类型拷贝范围。
        q.id = qec.id;
        q.ty = qec.ty;
        q.name = qec.name;
        q.flags = qec.flags;
        q.reserved = [0; 2];
        match CtrlType::try_from_u32(qec.ty) {
            Some(CtrlType::Integer | CtrlType::Boolean | CtrlType::Menu | CtrlType::Bitmask) => {
                q.minimum = qec.minimum as i32;
                q.maximum = qec.maximum as i32;
                q.step = qec.step as i32;
                q.default_value = qec.default_value as i32;
            }
            _ => {
                q.minimum = 0;
                q.maximum = 0;
                q.step = 0;
                q.default_value = 0;
            }
        }
        Ok(())
    }

    /// 处理 `VIDIOC_QUERYMENU`（对应 Linux `v4l2_querymenu`）。
    pub fn querymenu(&self, q: &mut Querymenu) -> Result<()> {
        let ctrl = self.find(q.id).ok_or(V4l2Error::InvalidArgument)?;
        if ctrl.ctrl_type != CtrlType::Menu {
            return Err(V4l2Error::InvalidArgument);
        }
        let qmenu = ctrl.qmenu.ok_or(V4l2Error::InvalidArgument)?;
        let i = q.index;
        if i < ctrl.minimum as u32 || i > ctrl.maximum as u32 {
            return Err(V4l2Error::InvalidArgument);
        }
        // 跳过掩码（Linux：menu_skip_mask 位 X 置位则跳过菜单项 X）。
        if i < 64 && (ctrl.step & (1u64 << i)) != 0 {
            return Err(V4l2Error::InvalidArgument);
        }
        let name = qmenu.get(i as usize).ok_or(V4l2Error::InvalidArgument)?;
        if name.is_empty() {
            return Err(V4l2Error::InvalidArgument);
        }
        let b = name.as_bytes();
        let len = b.len().min(q.name.len() - 1);
        q.name[..len].copy_from_slice(&b[..len]);
        q.name[len] = 0;
        q.reserved = 0;
        Ok(())
    }

    // ── 主线 G/S/TRY_EXT_CTRLS ────────────────────────────────────

    /// 处理 `VIDIOC_G_EXT_CTRLS`（对应 Linux `v4l2_g_ext_ctrls_common`）。
    ///
    /// `which` 支持当前值（CUR）、默认值（DEF）、最小值（MIN）、最大值（MAX）；
    /// `REQUEST_VAL` 未实现（无 media request），返回 `NotSupported`。
    pub fn g_ext_ctrls(&self, h: &mut ExtControls, cs: &mut [ExtControl]) -> Result<()> {
        let is_default = h.which == CTRL_WHICH_DEF_VAL;
        let is_request = h.which == CTRL_WHICH_REQUEST_VAL;
        let is_min = h.which == CTRL_WHICH_MIN_VAL;
        let is_max = h.which == CTRL_WHICH_MAX_VAL;

        h.error_idx = h.count;
        h.which = id2which(h.which);

        if is_request {
            return Err(V4l2Error::NotSupported);
        }
        if h.count == 0 {
            return self.class_check(h.which);
        }

        // 准备阶段：逐项查找并校验 which / DISABLED。
        let mut refs = Vec::with_capacity(cs.len());
        for (i, c) in cs.iter().enumerate() {
            h.error_idx = i as u32;
            let ctrl = self.prepare_ext_ctrl(h.which, c)?;
            refs.push(ctrl);
        }
        h.error_idx = h.count;

        // WRITE_ONLY 控件不可读（Linux：error_idx 保持 count）。
        if refs.iter().any(|c| c.flags.contains(CtrlFlags::WRITE_ONLY)) {
            return Err(V4l2Error::AccessDenied);
        }

        for (c, ctrl) in cs.iter_mut().zip(refs) {
            let v = if is_default {
                ctrl.default_value
            } else if is_min {
                ctrl.minimum
            } else if is_max {
                ctrl.maximum
            } else if ctrl.flags.contains(CtrlFlags::VOLATILE) {
                self.read_volatile(ctrl)?
            } else {
                ctrl.value()
            };
            write_ext_value(c, ctrl.ctrl_type, v);
        }
        h.error_idx = h.count;
        Ok(())
    }

    /// 处理 `VIDIOC_TRY_EXT_CTRLS`（对应 Linux `v4l2_try_ext_ctrls`）——
    /// 只校验并回写校验后的值，不应用。
    pub fn try_ext_ctrls(&self, h: &mut ExtControls, cs: &mut [ExtControl]) -> Result<()> {
        self.try_set_ext_ctrls(h, cs, false)
    }

    /// 处理 `VIDIOC_S_EXT_CTRLS`（对应 Linux `v4l2_s_ext_ctrls`）。
    ///
    /// 校验全部控件通过后应用；值变化触发 [`CtrlChangeNotify`]。
    pub fn s_ext_ctrls(&self, h: &mut ExtControls, cs: &mut [ExtControl]) -> Result<()> {
        self.try_set_ext_ctrls(h, cs, true)
    }

    /// `S_EXT_CTRLS` / `TRY_EXT_CTRLS` 共用实现（Linux `try_set_ext_ctrls_common`）。
    fn try_set_ext_ctrls(
        &self,
        h: &mut ExtControls,
        cs: &mut [ExtControl],
        set: bool,
    ) -> Result<()> {
        h.error_idx = h.count;

        // 默认 / 最小 / 最大值不可修改。
        if matches!(
            h.which,
            CTRL_WHICH_DEF_VAL | CTRL_WHICH_MIN_VAL | CTRL_WHICH_MAX_VAL
        ) {
            return Err(V4l2Error::InvalidArgument);
        }
        h.which = id2which(h.which);
        if h.which == CTRL_WHICH_REQUEST_VAL {
            return Err(V4l2Error::NotSupported);
        }
        if h.count == 0 {
            return self.class_check(h.which);
        }

        // 准备 + 校验：逐项解析为（控件, 校验后目标值）。
        // 对齐 Linux `prepare_ext_ctrls` + `validate_ctrls`：全部通过后才
        // 应用，保证"非法值不改变任何控件"。
        let mut resolved: Vec<(&Ctrl, i64)> = Vec::with_capacity(cs.len());
        for (i, c) in cs.iter().enumerate() {
            h.error_idx = i as u32;
            let ctrl = self.prepare_ext_ctrl(h.which, c)?;
            if ctrl.flags.contains(CtrlFlags::READ_ONLY) {
                return Err(V4l2Error::AccessDenied);
            }
            if set && ctrl.flags.contains(CtrlFlags::GRABBED) {
                return Err(V4l2Error::Busy);
            }
            let v = read_ext_value(c, ctrl.ctrl_type);
            let target = self.validate_new(ctrl, v)?;
            resolved.push((ctrl, target));
        }

        // 回写校验后的值（TRY 与 S 均回写，对齐 Linux `new_to_user`）。
        for (c, &(ctrl, target)) in cs.iter_mut().zip(&resolved) {
            write_ext_value(c, ctrl.ctrl_type, target);
        }

        // 应用阶段（仅 S）：调用 try/s_ctrl 回调并更新当前值。
        if set {
            for (c, &(ctrl, target)) in cs.iter_mut().zip(&resolved) {
                let new = self.apply_value(ctrl, target)?;
                write_ext_value(c, ctrl.ctrl_type, new);
            }
        }

        h.error_idx = h.count;
        Ok(())
    }

    // ── 弃用兼容 G/S_CTRL（Linux v4l2_g_ctrl / v4l2_s_ctrl）─────────

    /// 处理 `VIDIOC_G_CTRL`（弃用兼容，仅 `s32` 标量控件）。
    pub fn g_ctrl(&self, c: &mut Control) -> Result<()> {
        let ctrl = self.find(c.id).ok_or(V4l2Error::InvalidArgument)?;
        if !ctrl.ctrl_type.is_int() {
            return Err(V4l2Error::InvalidArgument);
        }
        if ctrl.flags.contains(CtrlFlags::WRITE_ONLY) {
            return Err(V4l2Error::AccessDenied);
        }
        let v = if ctrl.flags.contains(CtrlFlags::VOLATILE) {
            self.read_volatile(ctrl)?
        } else {
            ctrl.value()
        };
        c.value = v as i32;
        Ok(())
    }

    /// 处理 `VIDIOC_S_CTRL`（弃用兼容，仅 `s32` 标量控件）。
    ///
    /// 成功后 `c.value` 更新为实际生效值（可能经取整 / 设备 clamp）。
    pub fn s_ctrl(&self, c: &mut Control) -> Result<()> {
        let ctrl = self.find(c.id).ok_or(V4l2Error::InvalidArgument)?;
        if !ctrl.ctrl_type.is_int() {
            return Err(V4l2Error::InvalidArgument);
        }
        if ctrl.flags.contains(CtrlFlags::READ_ONLY) {
            return Err(V4l2Error::AccessDenied);
        }
        let target = self.validate_new(ctrl, c.value as i64)?;
        let new = self.apply_value(ctrl, target)?;
        c.value = new as i32;
        Ok(())
    }

    // ── 控件事件 ────────────────────────────────────────────────

    /// 处理 `VIDIOC_SUBSCRIBE_EVENT` 的 `V4L2_EVENT_CTRL` 订阅。
    pub fn subscribe_event(&self, fh: &mut V4l2Fh, sub: &EventSubscription) -> Result<()> {
        if sub.ty != EventType::Ctrl {
            return Err(V4l2Error::InvalidArgument);
        }
        let ctrl = self.find(sub.id).ok_or(V4l2Error::InvalidArgument)?;
        // 已订阅：幂等返回，不重复发送初始事件。
        if fh.is_subscribed(sub.ty, sub.id) {
            return Ok(());
        }
        fh.subscribe(sub, 0, EventOps::Ctrl)?;
        if sub.flags.contains(EventSubFlags::SEND_INITIAL) {
            let changes = if ctrl.flags.contains(CtrlFlags::WRITE_ONLY) {
                CtrlChange::FLAGS
            } else {
                CtrlChange::VALUE | CtrlChange::FLAGS
            };
            if let Some(ev) = self.fill_event(ctrl, changes) {
                fh.queue_event(ev);
            }
        }
        Ok(())
    }

    /// 构建控件值变化事件（供驱动按需投递）。
    pub fn change_event(&self, id: u32, changes: CtrlChange) -> Option<Event> {
        let ctrl = self.find(id)?;
        self.fill_event(ctrl, changes)
    }

    /// 填充 `V4L2_EVENT_CTRL` 载荷（对齐 Linux `v4l2-ctrls-core.c::fill_event`）。
    fn fill_event(&self, ctrl: &Ctrl, changes: CtrlChange) -> Option<Event> {
        Some(build_ctrl_event(
            CtrlEventParams {
                id: ctrl.id,
                ctrl_type: ctrl.ctrl_type as u32,
                value: ctrl.value(),
                flags: ctrl.flags.bits(),
                minimum: ctrl.minimum,
                maximum: ctrl.maximum,
                step: ctrl.step as i64,
                default_value: ctrl.default_value,
            },
            changes,
        ))
    }

    /// 值变化后触发通知回调（Linux `new_to_cur` → `send_event` 的角色）。
    fn emit_change(&self, ctrl: &Ctrl, changes: CtrlChange) {
        if let Some(notify) = &self.notify
            && let Some(ev) = self.fill_event(ctrl, changes)
        {
            notify(ev);
        }
    }

    // ── 内部辅助 ───────────────────────────────────────────────────

    /// 单个扩展控件的基础校验（Linux `prepare_ext_ctrls` 每项逻辑）。
    fn prepare_ext_ctrl(&self, which: u32, c: &ExtControl) -> Result<&Ctrl> {
        let id = c.id & CTRL_ID_MASK;
        let which_in_range = (CTRL_WHICH_DEF_VAL..=CTRL_WHICH_MAX_VAL).contains(&which);
        if which != 0 && !which_in_range && id2which(id) != which {
            return Err(V4l2Error::InvalidArgument);
        }
        // 旧式私有控件不允许用于扩展控件。
        if id >= CID_PRIVATE_BASE {
            return Err(V4l2Error::InvalidArgument);
        }
        let ctrl = self.find(id).ok_or(V4l2Error::InvalidArgument)?;
        if ctrl.flags.contains(CtrlFlags::DISABLED) {
            return Err(V4l2Error::InvalidArgument);
        }
        if !ctrl.flags.contains(CtrlFlags::HAS_WHICH_MIN_MAX)
            && (which == CTRL_WHICH_MIN_VAL || which == CTRL_WHICH_MAX_VAL)
        {
            return Err(V4l2Error::InvalidArgument);
        }
        Ok(ctrl)
    }

    /// `count == 0` 时的类检查（Linux `class_check`）。
    fn class_check(&self, which: u32) -> Result<()> {
        if which == 0 || (CTRL_WHICH_DEF_VAL..=CTRL_WHICH_MAX_VAL).contains(&which) {
            return Ok(());
        }
        if self.find(which | 1).is_some() {
            Ok(())
        } else {
            Err(V4l2Error::InvalidArgument)
        }
    }

    /// 读取 `VOLATILE` 控件的设备当前值（Linux `g_volatile_ctrl`）。
    fn read_volatile(&self, ctrl: &Ctrl) -> Result<i64> {
        if let Some(ops) = &ctrl.ops
            && let Some(get) = &ops.get
        {
            get()
        } else {
            Ok(ctrl.value())
        }
    }

    /// 类型校验 + 取整（Linux `std_validate_elem` + `try_ctrl` 回调）。
    fn validate_new(&self, ctrl: &Ctrl, v: i64) -> Result<i64> {
        let validated = match ctrl.ctrl_type {
            CtrlType::Integer | CtrlType::Integer64 => round_to_range(v, ctrl),
            CtrlType::Boolean => {
                if v != 0 {
                    1
                } else {
                    0
                }
            }
            CtrlType::Menu => {
                if v < ctrl.minimum || v > ctrl.maximum {
                    return Err(V4l2Error::OutOfRange);
                }
                if v < 64 && (ctrl.step & (1u64 << v)) != 0 {
                    return Err(V4l2Error::InvalidArgument);
                }
                if let Some(qmenu) = ctrl.qmenu {
                    let name = qmenu.get(v as usize).ok_or(V4l2Error::InvalidArgument)?;
                    if name.is_empty() {
                        return Err(V4l2Error::InvalidArgument);
                    }
                }
                v
            }
            CtrlType::Bitmask => v & ctrl.maximum,
            CtrlType::Button | CtrlType::CtrlClass => 0,
        };
        if let Some(ops) = &ctrl.ops
            && let Some(try_fn) = &ops.try_ctrl
        {
            return try_fn(validated);
        }
        Ok(validated)
    }

    /// 应用值（Linux `try_or_set_cluster`）：调用 `s_ctrl` 回调、更新当前值、
    /// 值变化时触发通知。
    fn apply_value(&self, ctrl: &Ctrl, v: i64) -> Result<i64> {
        let execute = ctrl.flags.contains(CtrlFlags::EXECUTE_ON_WRITE);
        // VOLATILE 控件：缓存值可能过期，必须每次写入设备（Linux
        // cluster_changed 排除 volatile 控件，但可设置的 volatile 控件
        // 仍需落设备，故此处显式放宽）。
        let always_write = execute || ctrl.flags.contains(CtrlFlags::VOLATILE);
        let cur = ctrl.value();
        if !always_write && cur == v {
            return Ok(v);
        }
        // `set` 为硬件控件必填回调；内存控件（`ops == None`）直接写 `cur`。
        let new = if let Some(ops) = &ctrl.ops {
            (ops.set)(v)?
        } else {
            v
        };
        if new != cur || execute {
            ctrl.set_value(new);
            self.emit_change(ctrl, CtrlChange::VALUE);
        }
        Ok(new)
    }
}

impl Default for CtrlHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// 通过引用迭代全部控件（`for c in &handler`），按 id 升序。
///
/// 迭代器为 `slice::Iter`（实现 `ExactSizeIterator` / `DoubleEndedIterator`）。
/// 控件元数据只读，故不提供 `&mut Ctrl` 迭代。
impl<'a> IntoIterator for &'a CtrlHandler {
    type Item = &'a Ctrl;
    type IntoIter = core::slice::Iter<'a, Ctrl>;

    fn into_iter(self) -> Self::IntoIter {
        self.ctrls.iter()
    }
}

// ── 模块级辅助 ─────────────────────────────────────────────────────

/// `V4L2_CTRL_ID2WHICH`：控件 ID → 所属类。
fn id2which(id: u32) -> u32 {
    id & 0x0fff_0000
}

/// NEXT_CTRL 枚举过滤：当前子集无 compound / 数组控件。
///
/// - `NEXT_CTRL` 单独使用：匹配所有控件（mask 过滤非 compound）；
/// - `NEXT_COMPOUND` 单独使用：不匹配任何控件（无 compound 控件）；
/// - 二者组合：匹配所有控件。
fn next_ctrl_match(next_compound_only: bool, next_all: bool) -> bool {
    next_all || !next_compound_only
}

/// 读取 `ExtControl` 的值（`Integer64` 走 `value64`，其余走 `value`）。
fn read_ext_value(c: &ExtControl, ty: CtrlType) -> i64 {
    if ty == CtrlType::Integer64 {
        // SAFETY: `ExtControlValue` 为 repr(C) 联合体，按 `value64` 成员读取。
        unsafe { c.value.value64 }
    } else {
        // SAFETY: 同上，按 `value`（i32）成员读取。
        (unsafe { c.value.value }) as i64
    }
}

/// 写回 `ExtControl` 的值（`Integer64` 走 `value64`，其余走 `value`）。
fn write_ext_value(c: &mut ExtControl, ty: CtrlType, v: i64) {
    if ty == CtrlType::Integer64 {
        // 写联合体字段为安全操作（读取才需要 unsafe）。
        c.value.value64 = v;
    } else {
        c.value.value = v as i32;
    }
}

/// 整数取整到合法范围（Linux `ROUND_TO_RANGE` 宏）。
fn round_to_range(v: i64, ctrl: &Ctrl) -> i64 {
    let step = ctrl.step as i64;
    let half = step / 2;
    let v = if ctrl.maximum >= 0 && v >= ctrl.maximum - half {
        ctrl.maximum
    } else {
        v + half
    };
    let v = v.clamp(ctrl.minimum, ctrl.maximum);
    let offset = v - ctrl.minimum;
    let offset = step * (offset / step);
    ctrl.minimum + offset
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::Ordering;

    use super::*;
    use crate::{
        ctrls::CtrlOps,
        interface::{
            Timespec,
            ctrl::{ExtControlValue, QueryCtrl},
            event::{EventCtrlPayload, EventSubFlags},
        },
    };

    const NEXT_CTRL: u32 = CTRL_NEXT_CTRL;
    const BRIGHTNESS: u32 = 0x0098_0900;
    const CONTRAST: u32 = 0x0098_0901;
    const TEST_PATTERN: u32 = 0x0098_0930;

    const TEST_PATTERN_MENU: &[&str] = &[
        "75% Colorbar",
        "100% Colorbar",
        "CSC Colorbar",
        "Black",
        "White",
    ];

    fn register_uvc_like(handler: &mut CtrlHandler) {
        // 乱序注册（对齐 UVC_CONTROL_DEFS 顺序：非 id 升序）。
        handler
            .new_int(0x0098_091C, "Backlight", 0, 255, 1, 0, None)
            .unwrap();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 0, None)
            .unwrap();
        handler
            .new_int(CONTRAST, "Contrast", 0, 255, 1, 0, None)
            .unwrap();
        handler
            .new_int(0x009A_0901, "ExposureAuto", 0, 4, 1, 0, None)
            .unwrap();
        handler
            .new_int(0x009A_0902, "ExposureAbs", 0, 10_000, 1, 0, None)
            .unwrap();
    }

    fn zero_query_ctrl() -> QueryCtrl {
        QueryCtrl {
            id: 0,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            reserved: [0; 2],
        }
    }

    fn ext_ctrl(id: u32, value: i32) -> ExtControl {
        ExtControl {
            id,
            size: 0,
            reserved2: [0; 1],
            value: ExtControlValue { value },
        }
    }

    fn ext_ctrl_i64(id: u32, value: i64) -> ExtControl {
        ExtControl {
            id,
            size: 0,
            reserved2: [0; 1],
            value: ExtControlValue { value64: value },
        }
    }

    fn ext_header(count: u32, which: u32) -> ExtControls {
        ExtControls {
            which,
            count,
            error_idx: 0,
            request_fd: 0,
            reserved: [0; 1],
            controls: 0,
        }
    }

    fn read_value(c: &ExtControl) -> i32 {
        // SAFETY: 测试中构造的控件均为非 Integer64 标量，读取 value 成员。
        unsafe { c.value.value }
    }

    /// NEXT_CTRL 枚举必须严格递增、返回 id 不得携带 NEXT 标志、
    /// 枚举完必须 EINVAL 终止。
    #[test]
    fn next_ctrl_enumeration_is_strictly_increasing_and_terminates() {
        let mut handler = CtrlHandler::new();
        register_uvc_like(&mut handler);

        let mut q = QueryExtCtrl {
            id: NEXT_CTRL,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            elem_size: 0,
            elems: 0,
            nr_of_dims: 0,
            dims: [0; 4],
            reserved: [0; 32],
        };
        let mut last_id = 0u32;
        let mut count = 0u32;
        loop {
            match handler.query_ext_ctrl(&mut q) {
                Ok(()) => {
                    assert_eq!(q.id & NEXT_CTRL, 0, "returned id carries NEXT flag");
                    assert!(q.id > last_id, "id not strictly increasing");
                    last_id = q.id;
                    count += 1;
                    q.id = last_id | NEXT_CTRL;
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

        let mut q = QueryExtCtrl {
            id: NEXT_CTRL,
            ty: 0,
            name: [0; 32],
            minimum: 0,
            maximum: 0,
            step: 0,
            default_value: 0,
            flags: CtrlFlags::empty(),
            elem_size: 0,
            elems: 0,
            nr_of_dims: 0,
            dims: [0; 4],
            reserved: [0; 32],
        };
        handler.query_ext_ctrl(&mut q).unwrap();
        assert_eq!(q.id, 0x0098_0900, "first control should be smallest id");

        q.id = 0x009A_0902 | NEXT_CTRL;
        assert!(matches!(
            handler.query_ext_ctrl(&mut q),
            Err(V4l2Error::InvalidArgument)
        ));
    }

    /// 容器 API：len / is_empty / iter / IntoIterator 覆盖全部控件（按 id 升序）。
    #[test]
    fn container_api_iterates_sorted_controls() {
        let mut handler = CtrlHandler::new();
        assert!(handler.is_empty());
        assert_eq!(handler.len(), 0);

        register_uvc_like(&mut handler);
        assert!(!handler.is_empty());
        assert_eq!(handler.len(), 5);

        let ids: Vec<u32> = handler.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            [
                0x0098_0900,
                0x0098_0901,
                0x0098_091C,
                0x009A_0901,
                0x009A_0902
            ]
        );

        // IntoIterator：for c in &handler。
        let via_for: Vec<u32> = (&handler).into_iter().map(|c| c.id).collect();
        assert_eq!(via_for, ids);

        // slice::Iter 自带 ExactSizeIterator / DoubleEndedIterator。
        assert_eq!(handler.iter().len(), 5);
        assert_eq!(handler.iter().next_back().map(|c| c.id), Some(0x009A_0902));
    }

    /// QUERYCTRL 由 QUERY_EXT_CTRL 换算：id/name/flags 与范围字段正确。
    #[test]
    fn queryctrl_derives_from_query_ext_ctrl() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let mut q = zero_query_ctrl();
        q.id = BRIGHTNESS;
        handler.queryctrl(&mut q).unwrap();
        assert_eq!(q.id, BRIGHTNESS);
        assert_eq!(q.ty, CtrlType::Integer as u32);
        assert_eq!(&q.name[..10], b"Brightness");
        assert_eq!(q.minimum, 0);
        assert_eq!(q.maximum, 255);
        assert_eq!(q.step, 1);
        assert_eq!(q.default_value, 128);
        assert!(q.flags.contains(CtrlFlags::HAS_WHICH_MIN_MAX));
    }

    /// QUERYMENU：越界索引 / 非菜单控件返回 EINVAL，合法项返回名称。
    #[test]
    fn querymenu_resolves_menu_items() {
        let mut handler = CtrlHandler::new();
        handler
            .new_menu(TEST_PATTERN, "Test Pattern", 5, 0, TEST_PATTERN_MENU, None)
            .unwrap();

        let mut q = Querymenu {
            id: TEST_PATTERN,
            index: 0,
            name: [0; 32],
            reserved: 0,
        };
        handler.querymenu(&mut q).unwrap();
        assert_eq!(&q.name[..12], b"75% Colorbar");

        q.index = 3;
        handler.querymenu(&mut q).unwrap();
        assert_eq!(&q.name[..5], b"Black");

        q.index = 99;
        assert!(matches!(
            handler.querymenu(&mut q),
            Err(V4l2Error::InvalidArgument)
        ));
    }

    /// G_EXT_CTRLS：正常读取当前值；缺失控件时 error_idx 指向失败索引。
    #[test]
    fn g_ext_ctrls_reads_values_and_sets_error_idx() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 0)];
        let mut h = ext_header(1, 0);
        handler.g_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 128);
        assert_eq!(h.error_idx, 1, "success -> error_idx == count");

        let mut cs = [ext_ctrl(0xDEAD_BEEF, 0)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.g_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(h.error_idx, 0, "failure -> error_idx == failing index");
    }

    /// G_EXT_CTRLS with which=DEF_VAL 返回默认值。
    #[test]
    fn g_ext_ctrls_which_default_returns_default() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 0)];
        let mut h = ext_header(1, CTRL_WHICH_DEF_VAL);
        handler.g_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 128);
    }

    /// G_EXT_CTRLS 拒绝 WRITE_ONLY 控件（EACCES）。
    #[test]
    fn g_ext_ctrls_rejects_write_only() {
        let mut handler = CtrlHandler::new();
        handler
            .new_ctrl(CtrlConfig {
                id: 0x0098_0931,
                name: "Action",
                ctrl_type: CtrlType::Button,
                minimum: 0,
                maximum: 0,
                step: 0,
                default_value: 0,
                flags: CtrlFlags::empty(),
                qmenu: None,
                ops: None,
            })
            .unwrap();

        let mut cs = [ext_ctrl(0x0098_0931, 0)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.g_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::AccessDenied)
        ));
    }

    /// S_EXT_CTRLS：整数按步长取整；越界 clamp 到 [min, max]。
    #[test]
    fn s_ext_ctrls_rounds_to_step() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 100, 10, 0, None)
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 55)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 60);
        assert_eq!(handler.value(BRIGHTNESS), Some(60));

        let mut cs = [ext_ctrl(BRIGHTNESS, 1000)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 100);
    }

    /// S_EXT_CTRLS：菜单越界返回 EINVAL 且不改变任何控件。
    #[test]
    fn s_ext_ctrls_rejects_bad_menu_value() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        handler
            .new_menu(TEST_PATTERN, "Test Pattern", 5, 0, TEST_PATTERN_MENU, None)
            .unwrap();

        let mut cs = [ext_ctrl(TEST_PATTERN, 99)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.s_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::OutOfRange)
        ));
        assert_eq!(handler.value(TEST_PATTERN), Some(0));
    }

    /// S_EXT_CTRLS：READ_ONLY 控件返回 EACCES。
    #[test]
    fn s_ext_ctrls_rejects_read_only() {
        let mut handler = CtrlHandler::new();
        handler
            .new_ctrl(CtrlConfig {
                id: BRIGHTNESS,
                name: "Readonly",
                ctrl_type: CtrlType::Integer,
                minimum: 0,
                maximum: 255,
                step: 1,
                default_value: 0,
                flags: CtrlFlags::READ_ONLY,
                qmenu: None,
                ops: None,
            })
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 10)];
        let mut h = ext_header(1, 0);
        assert!(matches!(
            handler.s_ext_ctrls(&mut h, &mut cs),
            Err(V4l2Error::AccessDenied)
        ));
    }

    /// TRY_EXT_CTRLS：回写校验后的值，但不应用（不改变当前值）。
    #[test]
    fn try_ext_ctrls_validates_without_applying() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 100, 10, 0, None)
            .unwrap();

        let mut cs = [ext_ctrl(BRIGHTNESS, 55)];
        let mut h = ext_header(1, 0);
        handler.try_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(read_value(&cs[0]), 60, "try 回写校验后的值");
        assert_eq!(handler.value(BRIGHTNESS), Some(0), "try 不应用");
    }

    /// S_EXT_CTRLS：值变化触发 change notify 回调（事件载荷）。
    #[test]
    fn s_ext_ctrls_fires_notify_on_change() {
        use alloc::sync::Arc;
        use core::sync::atomic::{AtomicU32, AtomicUsize};

        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let last_id = Arc::new(AtomicU32::new(0));
        let cnt = Arc::clone(&count);
        let id = Arc::clone(&last_id);
        handler.set_change_notify(Box::new(move |ev| {
            cnt.fetch_add(1, Ordering::Relaxed);
            id.store(ev.id, Ordering::Relaxed);
        }));

        let mut cs = [ext_ctrl(BRIGHTNESS, 200)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(last_id.load(Ordering::Relaxed), BRIGHTNESS);

        // 未变化：不触发。
        let mut cs = [ext_ctrl(BRIGHTNESS, 200)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    /// 弃用兼容 S_CTRL：取整并回写实际值。
    #[test]
    fn s_ctrl_rounds_and_writes_back() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 100, 10, 0, None)
            .unwrap();

        let mut c = Control {
            id: BRIGHTNESS,
            value: 55,
        };
        handler.s_ctrl(&mut c).unwrap();
        assert_eq!(c.value, 60);
        assert_eq!(handler.value(BRIGHTNESS), Some(60));
    }

    /// 弃用兼容 G_CTRL：返回当前值。
    #[test]
    fn g_ctrl_reads_current() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();

        let mut c = Control {
            id: BRIGHTNESS,
            value: 0,
        };
        handler.g_ctrl(&mut c).unwrap();
        assert_eq!(c.value, 128);
    }

    /// INTEGER64 控件走 value64；G/S_CTRL 拒绝 Integer64（非 is_int）。
    #[test]
    fn integer64_uses_value64_and_rejects_g_ctrl() {
        let mut handler = CtrlHandler::new();
        handler
            .new_ctrl(CtrlConfig {
                id: 0x0098_0903,
                name: "Tstamp",
                ctrl_type: CtrlType::Integer64,
                minimum: 0,
                maximum: 1_000_000,
                step: 1,
                default_value: 42,
                flags: CtrlFlags::empty(),
                qmenu: None,
                ops: None,
            })
            .unwrap();

        // G_EXT_CTRLS 读 value64。
        let mut cs = [ext_ctrl_i64(0x0098_0903, 0)];
        let mut h = ext_header(1, 0);
        handler.g_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(unsafe { cs[0].value.value64 }, 42);

        // S_EXT_CTRLS 写 value64。
        let mut cs = [ext_ctrl_i64(0x0098_0903, 500)];
        let mut h = ext_header(1, 0);
        handler.s_ext_ctrls(&mut h, &mut cs).unwrap();
        assert_eq!(handler.value(0x0098_0903), Some(500));

        // 弃用 G/S_CTRL 拒绝 Integer64。
        let mut c = Control {
            id: 0x0098_0903,
            value: 0,
        };
        assert!(matches!(
            handler.g_ctrl(&mut c),
            Err(V4l2Error::InvalidArgument)
        ));
        assert!(matches!(
            handler.s_ctrl(&mut c),
            Err(V4l2Error::InvalidArgument)
        ));
    }

    // ── 控件事件 ────────────────────────────────────────────────────

    fn ctrl_sub(id: u32, flags: EventSubFlags) -> EventSubscription {
        EventSubscription {
            ty: EventType::Ctrl,
            id,
            flags,
            reserved: [0; 5],
        }
    }

    fn zero_event() -> Event {
        Event {
            ty: 0,
            pad: 0,
            data: [0; 64],
            pending: 0,
            sequence: 0,
            timestamp: Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            id: 0,
            reserved: [0; 8],
        }
    }

    fn read_ctrl(ev: &Event) -> EventCtrlPayload {
        let mut payload = [0u8; core::mem::size_of::<EventCtrlPayload>()];
        payload.copy_from_slice(&ev.data[..core::mem::size_of::<EventCtrlPayload>()]);
        // SAFETY: EventCtrlPayload 是 repr(C) POD，长度等于 size_of。
        unsafe { core::ptr::read_unaligned(payload.as_ptr() as *const EventCtrlPayload) }
    }

    /// SEND_INITIAL：订阅后立即投递初始事件。
    #[test]
    fn subscribe_with_send_initial_queues_initial_event() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        let mut fh = V4l2Fh::new();

        handler
            .subscribe_event(&mut fh, &ctrl_sub(BRIGHTNESS, EventSubFlags::SEND_INITIAL))
            .unwrap();
        assert_eq!(fh.pending(), 1, "SEND_INITIAL queues one initial event");

        let out = fh.dequeue().unwrap();
        assert_eq!(out.ty, EventType::Ctrl as u32);
        assert_eq!(out.id, BRIGHTNESS);
        assert_eq!(out.reserved, [0; 8], "reserved must be zeroed");
        let payload = read_ctrl(&out);
        assert_eq!(
            payload.changes,
            (CtrlChange::VALUE | CtrlChange::FLAGS).bits(),
            "initial event changes = VALUE|FLAGS"
        );
        assert_eq!(payload.value, 128, "initial event carries current value");
    }

    /// 订阅不存在的控件 ID 或非 CTRL 类型必须 EINVAL。
    #[test]
    fn subscribe_rejects_unknown_ctrl_and_non_ctrl_type() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        let mut fh = V4l2Fh::new();

        assert!(matches!(
            handler.subscribe_event(&mut fh, &ctrl_sub(0xDEAD_BEEF, EventSubFlags::empty())),
            Err(V4l2Error::InvalidArgument)
        ));
        assert!(matches!(
            handler.subscribe_event(
                &mut fh,
                &EventSubscription {
                    ty: EventType::Eos,
                    id: BRIGHTNESS,
                    flags: EventSubFlags::empty(),
                    reserved: [0; 5],
                }
            ),
            Err(V4l2Error::InvalidArgument)
        ));
        assert_eq!(fh.pending(), 0);
    }

    /// 未带 SEND_INITIAL 的订阅不投递初始事件。
    #[test]
    fn subscribe_without_send_initial_queues_nothing() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        let mut fh = V4l2Fh::new();

        handler
            .subscribe_event(&mut fh, &ctrl_sub(BRIGHTNESS, EventSubFlags::empty()))
            .unwrap();
        assert_eq!(fh.pending(), 0);
        assert_eq!(read_ctrl(&zero_event()).changes, 0);
    }

    /// 控件值变化后 change_event 构造的载荷带当前值。
    #[test]
    fn change_event_carries_new_value() {
        let mut handler = CtrlHandler::new();
        handler
            .new_int(BRIGHTNESS, "Brightness", 0, 255, 1, 128, None)
            .unwrap();
        handler
            .s_ctrl(&mut Control {
                id: BRIGHTNESS,
                value: 200,
            })
            .unwrap();
        let ev = handler.change_event(BRIGHTNESS, CtrlChange::VALUE).unwrap();
        assert_eq!(ev.id, BRIGHTNESS);
        assert_eq!(read_ctrl(&ev).value, 200);
        assert_eq!(read_ctrl(&ev).changes, CtrlChange::VALUE.bits());
    }

    /// 硬件代理控件：S 走 set 回调并记录实际值，G 走 get 回调（VOLATILE）。
    #[test]
    fn hardware_proxy_ctrl_uses_ops() {
        use alloc::sync::Arc;
        use core::sync::atomic::AtomicUsize;

        let device_val = Arc::new(AtomicI64::new(5));
        let set_calls = Arc::new(AtomicUsize::new(0));

        let get_dev = Arc::clone(&device_val);
        let set_dev = Arc::clone(&device_val);
        let set_cnt = Arc::clone(&set_calls);
        let ops = CtrlOps {
            get: Some(Box::new(move || Ok(get_dev.load(Ordering::Relaxed)))),
            try_ctrl: None,
            set: Box::new(move |v| {
                set_cnt.fetch_add(1, Ordering::Relaxed);
                set_dev.store(v.clamp(0, 255), Ordering::Relaxed);
                Ok(set_dev.load(Ordering::Relaxed))
            }),
        };
        let mut handler = CtrlHandler::new();
        handler
            .new_ctrl(CtrlConfig {
                id: BRIGHTNESS,
                name: "Brightness",
                ctrl_type: CtrlType::Integer,
                minimum: 0,
                maximum: 255,
                step: 1,
                default_value: 0,
                flags: CtrlFlags::VOLATILE,
                qmenu: None,
                ops: Some(ops),
            })
            .unwrap();

        let mut c = Control {
            id: BRIGHTNESS,
            value: 200,
        };
        handler.s_ctrl(&mut c).unwrap();
        assert_eq!(c.value, 200);
        assert_eq!(set_calls.load(Ordering::Relaxed), 1);

        let mut c = Control {
            id: BRIGHTNESS,
            value: 0,
        };
        handler.g_ctrl(&mut c).unwrap();
        assert_eq!(c.value, 200, "VOLATILE 控件 G 读取设备值");
    }
}
