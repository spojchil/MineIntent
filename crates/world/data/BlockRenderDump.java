// 从原版服务端导出成像需要、而 Azalea 方块数据里没有的方块状态属性。
//
// 每个状态 2 字节，按状态号顺序：
//   字节 0：发光值（getLightEmission，高 4 位）| 透光度（getLightDampening，低 4 位）
//   字节 1：位 0 isViewBlocking，位 1 isSolidRender，位 2 emissiveRendering，
//          位 3 isCollisionShapeFullBlock，位 4 propagatesSkylightDown
// 依赖位置的判定用空世界原点求值，与原版在空邻域下的结果一致。
//
// 重新生成（26.1.2 服务端 bundler 内层 jar 与其依赖库）：
//   unzip -q server.jar 'META-INF/libraries/*' -d lib
//   CP="server-inner.jar:$(find lib -name '*.jar' | tr '\n' ':')"
//   javac -cp "$CP" -d out BlockRenderDump.java
//   java -cp "$CP:out" BlockRenderDump block_render_26.1.2.bin
import java.io.FileOutputStream;

import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

public class BlockRenderDump {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var level = EmptyBlockGetter.INSTANCE;
        var pos = BlockPos.ZERO;
        int count = Block.BLOCK_STATE_REGISTRY.size();
        byte[] out = new byte[count * 2];
        for (int id = 0; id < count; id++) {
            BlockState state = Block.BLOCK_STATE_REGISTRY.byId(id);
            out[id * 2] = (byte) ((state.getLightEmission() << 4) | state.getLightDampening());
            int flags = (state.isViewBlocking(level, pos) ? 1 : 0)
                | (state.isSolidRender() ? 2 : 0)
                | (state.emissiveRendering(level, pos) ? 4 : 0)
                | (state.isCollisionShapeFullBlock(level, pos) ? 8 : 0)
                | (state.propagatesSkylightDown() ? 16 : 0);
            out[id * 2 + 1] = (byte) flags;
        }
        try (var file = new FileOutputStream(args[0])) {
            file.write(out);
        }
        System.out.println(count + " states");
    }
}
