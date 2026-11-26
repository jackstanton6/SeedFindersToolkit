import java.io.FileWriter;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.Fluids;
import net.minecraft.world.phys.shapes.Shapes;
import net.minecraft.world.phys.shapes.VoxelShape;

/// Per block state (by id) the predicates worldgen code asks for, from the game's own classes:
/// [flags, fluid (0 none, 1 water, 2 lava), fluid amount, source, falling]. Run by `python -m sft data`.
public class BlockProps {
    public static void main(String[] args) throws Exception {
        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        var g = EmptyBlockGetter.INSTANCE;
        var p = BlockPos.ZERO;
        StringBuilder sb = new StringBuilder("[");
        for (int id = 0; id < Block.BLOCK_STATE_REGISTRY.size(); id++) {
            BlockState s = Block.BLOCK_STATE_REGISTRY.byId(id);
            VoxelShape c = s.getCollisionShape(g, p);
            long f = 0;
            f |= s.isAir() ? 1L : 0;
            f |= s.isSolid() ? 1L << 1 : 0;
            f |= c == Shapes.block() ? 1L << 2 : 0;
            f |= c.isEmpty() ? 1L << 3 : 0;
            f |= s.canBeReplaced() ? 1L << 4 : 0;
            f |= s.isSolidRender() ? 1L << 5 : 0;
            f |= s.liquid() ? 1L << 6 : 0;
            f |= s.getBlock().hasDynamicShape() ? 1L << 8 : 0;
            f |= s.isCollisionShapeFullBlock(g, p) ? 1L << 9 : 0;
            f |= s.ignitedByLava() ? 1L << 10 : 0;
            for (Direction d : Direction.values()) {
                f |= s.isFaceSturdy(g, p, d) ? 1L << (16 + d.ordinal()) : 0;
                f |= Block.isFaceFull(c, d) ? 1L << (24 + d.ordinal()) : 0;
            }
            var fl = s.getFluidState();
            int type = fl.isEmpty() ? 0 : fl.getType().isSame(Fluids.WATER) ? 1 : 2;
            int falling = !fl.isEmpty() && fl.hasProperty(net.minecraft.world.level.material.FlowingFluid.FALLING) && fl.getValue(net.minecraft.world.level.material.FlowingFluid.FALLING) ? 1 : 0;
            sb.append(id > 0 ? "," : "").append('[').append(f).append(',').append(type).append(',').append(fl.getAmount())
                .append(',').append(fl.isSource() ? 1 : 0).append(',').append(falling).append(']');
        }
        sb.append(']');
        try (FileWriter w = new FileWriter(args[0])) {
            w.write(sb.toString());
        }
        // each block's class (its canSurvive, placement and shape rules live there)
        StringBuilder cl = new StringBuilder("{");
        boolean first = true;
        for (Block b : net.minecraft.core.registries.BuiltInRegistries.BLOCK) {
            cl.append(first ? "" : ",").append('"').append(net.minecraft.core.registries.BuiltInRegistries.BLOCK.getKey(b)).append("\":\"")
                .append(b.getClass().getSimpleName()).append('"');
            first = false;
        }
        try (FileWriter w = new FileWriter(args[1])) {
            w.write(cl.append('}').toString());
        }
    }
}
