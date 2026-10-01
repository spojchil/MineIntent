// 从 26.1.2 客户端导出方块实体（箱子、床、潜影盒、钟、讲台与附魔台上的书）的几何。
//
// 不抄数值：模型来自原版 LayerDefinitions.createRoots()，摆放变换调原版渲染器的静态方法，
// 动画姿态调原版 setupAnim，顶点由原版 ModelPart.render 写进这里的录制器。这里只按各渲染器
// submit 的流程（选哪个模型、哪个变换、哪张贴图）把它们串起来，并取静止状态：
// 盖子关闭（openness 0）、钟不摇、附魔台的书时间 0 且无人靠近（合着、朝向 0）。
//
// 输出 JSON：
//   blocks：方块名 → {properties: 决定几何的属性名（升序）, variants: "k=v,..." → {texture, geometry}}
//   geometries：几何名 → 四边形数组，每个 [[x,y,z,u,v] ×4, [nx,ny,nz]]；坐标是方块内 0..1，
//   UV 是贴图比例 0..1，法线已过原版姿态的法线矩阵。
//
// 重新生成（26.1.2 客户端 jar 与其依赖库，库路径取自版本 json）：
//   CP="client.jar:$(find libraries -name '*.jar' | tr '\n' ':')"
//   javac -cp "$CP" -d out BlockEntityModelDump.java
//   java -cp "$CP:out" BlockEntityModelDump block_entity_models_26.1.2.json
import java.io.FileWriter;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TreeMap;
import java.util.function.Consumer;

import com.mojang.blaze3d.vertex.PoseStack;
import com.mojang.blaze3d.vertex.VertexConsumer;
import com.mojang.math.Axis;

import net.minecraft.SharedConstants;
import net.minecraft.client.model.Model;
import net.minecraft.client.model.geom.LayerDefinitions;
import net.minecraft.client.model.geom.ModelLayerLocation;
import net.minecraft.client.model.geom.ModelLayers;
import net.minecraft.client.model.geom.ModelPart;
import net.minecraft.client.model.geom.builders.LayerDefinition;
import net.minecraft.client.model.object.bell.BellModel;
import net.minecraft.client.model.object.book.BookModel;
import net.minecraft.client.model.object.chest.ChestModel;
import net.minecraft.client.renderer.Sheets;
import net.minecraft.client.renderer.blockentity.BedRenderer;
import net.minecraft.client.renderer.blockentity.BellRenderer;
import net.minecraft.client.renderer.blockentity.ChestRenderer;
import net.minecraft.client.renderer.blockentity.EnchantTableRenderer;
import net.minecraft.client.renderer.blockentity.ShulkerBoxRenderer;
import net.minecraft.client.renderer.blockentity.state.ChestRenderState.ChestMaterialType;
import net.minecraft.client.resources.model.sprite.SpriteId;
import net.minecraft.core.Direction;
import net.minecraft.server.Bootstrap;
import net.minecraft.util.Mth;
import net.minecraft.world.item.DyeColor;
import net.minecraft.world.level.block.state.properties.BedPart;
import net.minecraft.world.level.block.state.properties.ChestType;

public class BlockEntityModelDump {
    /** 录下 ModelPart.render 写出的顶点：位置、UV、法线。 */
    static final class Recorder implements VertexConsumer {
        final List<float[]> vertices = new ArrayList<>();
        float[] current;

        public VertexConsumer addVertex(float x, float y, float z) {
            current = new float[] {x, y, z, 0, 0, 0, 0, 0};
            vertices.add(current);
            return this;
        }
        public VertexConsumer setColor(int r, int g, int b, int a) { return this; }
        public VertexConsumer setColor(int argb) { return this; }
        public VertexConsumer setUv(float u, float v) { current[3] = u; current[4] = v; return this; }
        public VertexConsumer setUv1(int u, int v) { return this; }
        public VertexConsumer setUv2(int u, int v) { return this; }
        public VertexConsumer setNormal(float x, float y, float z) {
            current[5] = x; current[6] = y; current[7] = z;
            return this;
        }
        public VertexConsumer setLineWidth(float width) { return this; }
    }

    static Map<ModelLayerLocation, LayerDefinition> roots;
    static final Map<String, Map<String, Object>> blocks = new TreeMap<>();
    static final Map<String, String> geometries = new TreeMap<>();

    static ModelPart bake(ModelLayerLocation layer) {
        return roots.get(layer).bakeRoot();
    }

    static String number(float value) {
        float rounded = Math.round(value * 1e6f) / 1e6f;
        if (rounded == 0) rounded = 0; // 去掉 -0
        return String.format(Locale.ROOT, "%s", rounded);
    }

    /** 按 setup 摆好姿态，render 一棵部件树，存成几何。 */
    static void geometry(String name, Consumer<PoseStack> setup, ModelPart root) {
        if (geometries.containsKey(name)) return;
        PoseStack pose = new PoseStack();
        setup.accept(pose);
        Recorder recorder = new Recorder();
        root.render(pose, recorder, 0xF000F0, 0);
        StringBuilder out = new StringBuilder("[");
        for (int q = 0; q < recorder.vertices.size(); q += 4) {
            if (q > 0) out.append(',');
            out.append("[[");
            for (int i = 0; i < 4; i++) {
                float[] v = recorder.vertices.get(q + i);
                if (i > 0) out.append(',');
                out.append('[').append(number(v[0])).append(',').append(number(v[1])).append(',')
                    .append(number(v[2])).append(',').append(number(v[3])).append(',')
                    .append(number(v[4])).append(']');
            }
            float[] n = recorder.vertices.get(q);
            out.append("],[").append(number(n[5])).append(',').append(number(n[6])).append(',')
                .append(number(n[7])).append("]]");
        }
        geometries.put(name, out.append(']').toString());
    }

    @SuppressWarnings("unchecked")
    static void variant(String block, List<String> properties, String key, SpriteId sprite, String geometry) {
        Map<String, Object> entry = blocks.computeIfAbsent(block, b -> {
            Map<String, Object> m = new LinkedHashMap<>();
            m.put("properties", properties);
            m.put("variants", new TreeMap<String, Map<String, String>>());
            return m;
        });
        Map<String, String> v = new LinkedHashMap<>();
        v.put("texture", sprite.texture().toString());
        v.put("geometry", geometry);
        ((Map<String, Map<String, String>>) entry.get("variants")).put(key, v);
    }

    static Object staticField(Class<?> owner, String name) throws Exception {
        Field field = owner.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(null);
    }

    static final Direction[] HORIZONTAL = {Direction.NORTH, Direction.SOUTH, Direction.WEST, Direction.EAST};

    static String lower(Enum<?> value) {
        return value.name().toLowerCase(Locale.ROOT);
    }

    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        roots = LayerDefinitions.createRoots();

        // ---- 箱子（ChestRenderer）：modelTransformation(facing)，按 type 选模型层，setupAnim(open=0)。
        Object chestLayers = staticField(ChestRenderer.class, "LAYERS");
        var select = chestLayers.getClass().getMethod("select", ChestType.class);
        Map<String, ChestMaterialType> chests = new LinkedHashMap<>();
        chests.put("chest", ChestMaterialType.REGULAR);
        chests.put("trapped_chest", ChestMaterialType.TRAPPED);
        chests.put("copper_chest", ChestMaterialType.COPPER_UNAFFECTED);
        chests.put("exposed_copper_chest", ChestMaterialType.COPPER_EXPOSED);
        chests.put("weathered_copper_chest", ChestMaterialType.COPPER_WEATHERED);
        chests.put("oxidized_copper_chest", ChestMaterialType.COPPER_OXIDIZED);
        chests.put("ender_chest", ChestMaterialType.ENDER_CHEST);
        for (var chest : chests.entrySet()) {
            boolean ender = chest.getValue() == ChestMaterialType.ENDER_CHEST;
            ChestType[] types = ender ? new ChestType[] {ChestType.SINGLE} : ChestType.values();
            List<String> properties = ender ? List.of("facing") : List.of("facing", "type");
            for (ChestType type : types) {
                for (Direction facing : HORIZONTAL) {
                    String geometry = "chest/" + lower(type) + "/" + lower(facing);
                    ChestModel model = new ChestModel(bake((ModelLayerLocation) select.invoke(chestLayers, type)));
                    model.setupAnim(0.0f);
                    geometry(geometry, pose -> pose.mulPose(ChestRenderer.modelTransformation(facing)), model.root());
                    String key = ender ? "facing=" + lower(facing) : "facing=" + lower(facing) + ",type=" + lower(type);
                    variant(chest.getKey(), properties, key, Sheets.chooseSprite(chest.getValue(), type), geometry);
                    if (chest.getKey().contains("copper")) {
                        variant("waxed_" + chest.getKey(), properties, key, Sheets.chooseSprite(chest.getValue(), type), geometry);
                    }
                }
            }
        }

        // ---- 床（BedRenderer）：modelTransform(facing)，按 part 选头/脚模型层。
        for (DyeColor color : DyeColor.values()) {
            for (BedPart part : BedPart.values()) {
                for (Direction facing : HORIZONTAL) {
                    String geometry = "bed/" + lower(part) + "/" + lower(facing);
                    ModelPart root = bake(part == BedPart.HEAD ? ModelLayers.BED_HEAD : ModelLayers.BED_FOOT);
                    geometry(geometry, pose -> pose.mulPose(BedRenderer.modelTransform(facing)), root);
                    variant(color.getSerializedName() + "_bed", List.of("facing", "part"),
                        "facing=" + lower(facing) + ",part=" + lower(part), Sheets.getBedSprite(color), geometry);
                }
            }
        }

        // ---- 潜影盒（ShulkerBoxRenderer）：modelTransform(facing)，ShulkerBoxModel.setupAnim(progress=0)。
        Class<?> boxModel = Class.forName("net.minecraft.client.renderer.blockentity.ShulkerBoxRenderer$ShulkerBoxModel");
        Constructor<?> boxConstructor = boxModel.getDeclaredConstructor(ModelPart.class);
        boxConstructor.setAccessible(true);
        for (Direction facing : Direction.values()) {
            String geometry = "shulker_box/" + lower(facing);
            @SuppressWarnings("unchecked")
            Model<Float> model = (Model<Float>) boxConstructor.newInstance(bake(ModelLayers.SHULKER_BOX));
            model.setupAnim(0.0f);
            geometry(geometry, pose -> pose.mulPose(ShulkerBoxRenderer.modelTransform(facing)), model.root());
            variant("shulker_box", List.of("facing"), "facing=" + lower(facing),
                Sheets.DEFAULT_SHULKER_TEXTURE_LOCATION, geometry);
            for (DyeColor color : DyeColor.values()) {
                variant(color.getSerializedName() + "_shulker_box", List.of("facing"), "facing=" + lower(facing),
                    Sheets.getShulkerBoxSprite(color), geometry);
            }
        }

        // ---- 钟（BellRenderer）：不加变换，BellModel.setupAnim(ticks=0, 不摇)。
        BellModel bell = new BellModel(bake(ModelLayers.BELL));
        bell.setupAnim(new BellModel.State(0.0f, null));
        geometry("bell", pose -> {}, bell.root());
        variant("bell", List.of(), "", BellRenderer.BELL_TEXTURE, "bell");

        // ---- 讲台上的书（LecternRenderer）：只在 has_book=true 时画。
        BookModel.State lecternBook = (BookModel.State) staticField(
            net.minecraft.client.renderer.blockentity.LecternRenderer.class, "BOOK_STATE");
        for (Direction facing : HORIZONTAL) {
            String geometry = "lectern_book/" + lower(facing);
            BookModel book = new BookModel(bake(ModelLayers.BOOK));
            book.setupAnim(lecternBook);
            float yRot = facing.getClockWise().toYRot();
            geometry(geometry, pose -> {
                pose.translate(0.5f, 1.0625f, 0.5f);
                pose.mulPose(Axis.YP.rotationDegrees(-yRot));
                pose.mulPose(Axis.ZP.rotationDegrees(67.5f));
                pose.translate(0.0f, -0.125f, 0.0f);
            }, book.root());
            variant("lectern", List.of("facing", "has_book"), "facing=" + lower(facing) + ",has_book=true",
                EnchantTableRenderer.BOOK_TEXTURE, geometry);
        }

        // ---- 附魔台上的书（EnchantTableRenderer）：静止取 time=0、flip=0、open=0、yRot=0。
        float time = 0.0f, flip = 0.0f, open = 0.0f, yRot = 0.0f;
        BookModel book = new BookModel(bake(ModelLayers.BOOK));
        float page1 = Mth.frac(flip + 0.25f) * 1.6f - 0.3f;
        float page2 = Mth.frac(flip + 0.75f) * 1.6f - 0.3f;
        book.setupAnim(BookModel.State.forAnimation(time, Mth.clamp(page1, 0.0f, 1.0f), Mth.clamp(page2, 0.0f, 1.0f), open));
        geometry("enchanting_table_book", pose -> {
            pose.translate(0.5f, 0.75f, 0.5f);
            pose.translate(0.0f, 0.1f + Mth.sin(time * 0.1f) * 0.01f, 0.0f);
            pose.mulPose(Axis.YP.rotation(-yRot));
            pose.mulPose(Axis.ZP.rotationDegrees(80.0f));
        }, book.root());
        variant("enchanting_table", List.of(), "", EnchantTableRenderer.BOOK_TEXTURE, "enchanting_table_book");

        try (FileWriter out = new FileWriter(args[0])) {
            out.write("{\"blocks\":{");
            boolean first = true;
            for (var block : blocks.entrySet()) {
                if (!first) out.write(',');
                first = false;
                out.write('"' + block.getKey() + "\":{\"properties\":[");
                @SuppressWarnings("unchecked")
                List<String> properties = (List<String>) block.getValue().get("properties");
                out.write(String.join(",", properties.stream().map(p -> '"' + p + '"').toList()));
                out.write("],\"variants\":{");
                @SuppressWarnings("unchecked")
                Map<String, Map<String, String>> variants = (Map<String, Map<String, String>>) block.getValue().get("variants");
                boolean firstVariant = true;
                for (var v : variants.entrySet()) {
                    if (!firstVariant) out.write(',');
                    firstVariant = false;
                    out.write('"' + v.getKey() + "\":{\"texture\":\"" + v.getValue().get("texture")
                        + "\",\"geometry\":\"" + v.getValue().get("geometry") + "\"}");
                }
                out.write("}}");
            }
            out.write("},\"geometries\":{");
            first = true;
            for (var g : geometries.entrySet()) {
                if (!first) out.write(',');
                first = false;
                out.write('"' + g.getKey() + "\":" + g.getValue());
            }
            out.write("}}\n");
        }
    }
}
