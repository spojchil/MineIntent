use crate::minecraft::{BoxFuture, OperationControl, ViewportRead};

use super::{InformationError, InformationScopeSnapshot, PassiveObservations};

/// B 的唯一 information 入口。实现负责权限和 DTO omission，不向 B 暴露 provider SPI。
pub trait InformationFacade: Send + Sync {
    fn scope_snapshot(&self) -> InformationScopeSnapshot;

    fn compose_passive_observations(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<PassiveObservations, InformationError>>;

    /// 返回 backend kernel 的原子完整读；不得拆成 projection/source/revision 三次查询。
    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ViewportRead, InformationError>>;
}
