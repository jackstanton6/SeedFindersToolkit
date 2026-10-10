// Ground-truth dumper: drives the real Minecraft worldgen classes so the Rust port can be diffed.
// Run via tests/run_oracle.py (builds the server-jar classpath), e.g.
//   java -cp <server jar + libraries> oracle/Oracle.java climate 12345 2000 out.json
import java.io.FileWriter;
import java.util.Random;
import net.minecraft.SharedConstants;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeResolver;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.NoiseRouter;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.densityfunction.DensityFunction;
import net.minecraft.world.level.levelgen.densityfunction.SamplerContext;

public class Oracle {
    static String key(Holder<Biome> b) {
        return b.unwrapKey().orElseThrow().identifier().toString();
    }

    /// Every structure start in chunks [-radius, radius]^2, decided like ChunkGenerator.createStructures.
    /// Registries are loaded from the vanilla pack the way WorldLoader does, so template pools,
    /// structure templates and block tags are all real.
    static void structures(StringBuilder sb, long seed, int radius) throws Exception {
        var repo = net.minecraft.server.packs.repository.ServerPacksSource.createVanillaTrustedRepository();
        repo.reload();
        repo.setSelected(java.util.List.of("vanilla"));
        var rm = new net.minecraft.server.packs.resources.MultiPackResourceManager(net.minecraft.server.packs.PackType.SERVER_DATA, repo.openAllSelected());
        var layers = net.minecraft.server.RegistryLayer.createRegistryAccess();
        var staticTags = net.minecraft.tags.TagLoader.loadTagsForExistingRegistries(rm, layers.getLayer(net.minecraft.server.RegistryLayer.STATIC));
        var ctx = net.minecraft.tags.TagLoader.buildUpdatedLookups(layers.getAccessForLoading(net.minecraft.server.RegistryLayer.WORLD), staticTags);
        var world = net.minecraft.resources.RegistryDataLoader.load(rm, ctx, net.minecraft.resources.RegistryDataLoader.WORLD_REGISTRIES, Runnable::run).join();
        staticTags.forEach(net.minecraft.core.Registry.PendingTags::apply);

        Holder<NoiseGeneratorSettings> settings = world.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        RandomState rs = RandomState.create(world.lookupOrThrow(Registries.NOISE), seed, settings.value());
        MultiNoiseBiomeSource source = MultiNoiseBiomeSource.createFromPreset(
            world.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST).getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        NoiseBasedChunkGenerator gen = new NoiseBasedChunkGenerator(source, settings);
        LevelHeightAccessor level = LevelHeightAccessor.create(-64, 384);
        var storage = net.minecraft.world.level.storage.LevelStorageSource.createDefault(java.nio.file.Path.of("oracle-levels")).createAccess("oracle");
        var templates = new net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager(
            rm, storage, net.minecraft.util.datafix.DataFixers.getDataFixer(), layers.getLayer(net.minecraft.server.RegistryLayer.STATIC).lookupOrThrow(Registries.BLOCK));
        var state = net.minecraft.world.level.chunk.ChunkGeneratorStructureState.createForNormal(
            rs, seed, new net.minecraft.world.level.ChunkPos(0, 0), source, world.lookupOrThrow(Registries.STRUCTURE_SET));

        sb.append(",\"radius\":").append(radius).append(",\"starts\":[");
        boolean first = true;
        for (var setHolder : state.possibleStructureSets()) {
            var set = setHolder.value();
            String setName = setHolder.unwrapKey().orElseThrow().identifier().toString();
            for (int cx = -radius; cx <= radius; cx++) for (int cz = -radius; cz <= radius; cz++) {
                if (!set.placement().isStructureChunk(state, cx, cz)) continue;
                var chunk = new net.minecraft.world.level.ChunkPos(cx, cz);
                var sampler = rs.createClimateSampler(SamplerContext.builder().enableCaches().build());
                var options = new java.util.ArrayList<>(set.structures());
                String hit = null;
                net.minecraft.core.BlockPos pos = null;
                var random = new net.minecraft.world.level.levelgen.WorldgenRandom(new net.minecraft.world.level.levelgen.LegacyRandomSource(0L));
                random.setLargeFeatureSeed(seed, cx, cz);
                int total = options.stream().mapToInt(e -> e.weight()).sum();
                while (!options.isEmpty()) {
                    int index = 0;
                    if (options.size() > 1 || set.structures().size() > 1) {
                        int choice = random.nextInt(total);
                        for (var o : options) { choice -= o.weight(); if (choice < 0) break; index++; }
                    }
                    var selected = options.get(index);
                    var structure = selected.structure().value();
                    var gc = new net.minecraft.world.level.levelgen.structure.Structure.GenerationContext(
                        world, gen, source, sampler, rs, templates, seed, chunk, level, structure.biomes()::contains);
                    var stub = structure.findValidGenerationPoint(gc);
                    if (stub.isPresent()) {
                        hit = selected.structure().unwrapKey().orElseThrow().identifier().toString();
                        pos = stub.get().position();
                        break;
                    }
                    options.remove(index);
                    total -= selected.weight();
                }
                if (!first) sb.append(',');
                first = false;
                sb.append("[\"").append(setName).append("\",").append(cx).append(',').append(cz);
                if (hit != null) sb.append(",\"").append(hit).append("\",").append(pos.getX()).append(',').append(pos.getY()).append(',').append(pos.getZ());
                sb.append(']');
            }
        }
        sb.append("],\"rings\":[");
        first = true;
        for (var setHolder : state.possibleStructureSets()) {
            if (setHolder.value().placement() instanceof net.minecraft.world.level.levelgen.structure.placement.ConcentricRingsStructurePlacement rings) {
                for (var c : state.getRingPositionsFor(rings)) {
                    if (!first) sb.append(',');
                    first = false;
                    sb.append('[').append(c.x()).append(',').append(c.z()).append(']');
                }
            }
        }
        sb.append(']');
    }

    public static void main(String[] args) throws Exception {
        String mode = args[0];
        long seed = Long.parseLong(args[1]);
        int count = Integer.parseInt(args[2]);
        String out = args[3];

        SharedConstants.tryDetectVersion();
        Bootstrap.bootStrap();
        HolderLookup.Provider lookup = VanillaRegistries.createWorldLookup();
        Holder<NoiseGeneratorSettings> settings = lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        RandomState rs = RandomState.create(lookup.lookupOrThrow(Registries.NOISE), seed, settings.value());
        MultiNoiseBiomeSource source = MultiNoiseBiomeSource.createFromPreset(
            lookup.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST).getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        NoiseBasedChunkGenerator gen = new NoiseBasedChunkGenerator(source, settings);
        LevelHeightAccessor level = LevelHeightAccessor.create(-64, 384);
        NoiseRouter r = settings.value().noiseRouter();
        Random rand = new Random(seed ^ 0x5EED);
        StringBuilder sb = new StringBuilder("{\"seed\":").append(seed);

        if (mode.equals("climate")) {
            BiomeResolver resolver = source.createResolver(rs.createClimateSampler(SamplerContext.EMPTY_UNCACHED));
            DensityFunction[] climate = {r.temperature(), r.vegetation(), r.continents(), r.erosion(), r.depth(), r.ridges()};
            sb.append(",\"points\":[");
            for (int i = 0; i < count; i++) {
                // mostly near spawn, some far out
                int range = i % 4 == 0 ? 2_000_000 : 20_000;
                int qx = rand.nextInt(range) - range / 2, qz = rand.nextInt(range) - range / 2, qy = rand.nextInt(96) - 16;
                int x = qx << 2, y = qy << 2, z = qz << 2;
                if (i > 0) sb.append(',');
                sb.append('[').append(qx).append(',').append(qy).append(',').append(qz);
                for (DensityFunction f : climate) {
                    sb.append(',').append(rs.sampleBlockValueUncached(f, x, y, z));
                }
                sb.append(",\"").append(key(resolver.getNoiseBiome(qx, qy, qz))).append("\"]");
            }
            sb.append(']');
        } else if (mode.equals("chunkbiomes")) {
            // createResolverForChunk = volume path, what generated chunks store
            sb.append(",\"chunks\":[");
            for (int i = 0; i < count; i++) {
                int cx = rand.nextInt(4000) - 2000, cz = rand.nextInt(4000) - 2000;
                BiomeResolver res = source.createResolverForChunk(
                    rs.createClimateSampler(SamplerContext.builder().enableCaches().build()), cx * 4, -16, cz * 4, 4, 96, 4);
                if (i > 0) sb.append(',');
                sb.append('[').append(cx).append(',').append(cz).append(",[");
                boolean first = true;
                for (int qz = 0; qz < 4; qz++) for (int qx = 0; qx < 4; qx++) for (int qy = 0; qy < 96; qy++) {
                    if (!first) sb.append(',');
                    first = false;
                    sb.append('"').append(key(res.getNoiseBiome(cx * 4 + qx, qy - 16, cz * 4 + qz))).append('"');
                }
                sb.append("]]");
            }
            sb.append(']');
        } else if (mode.equals("heights")) {
            sb.append(",\"heights\":[");
            for (int i = 0; i < count; i++) {
                int range = i % 4 == 0 ? 200_000 : 4_000;
                int x = rand.nextInt(range) - range / 2, z = rand.nextInt(range) - range / 2;
                if (i > 0) sb.append(',');
                // OCEAN_FLOOR_WG is tag-driven (blocks_motion_in_heightmap) and tags are unbound here;
                // a noise column only holds stone/water/lava/air, of which only stone is in the tag.
                var column = gen.getBaseColumn(x, z, level, rs);
                int floor = -64;
                for (int y = 319; y >= -64; y--) {
                    if (column.getBlock(y).is(net.minecraft.world.level.block.Blocks.STONE)) { floor = y + 1; break; }
                }
                sb.append('[').append(x).append(',').append(z).append(',')
                    .append(gen.getBaseHeight(x, z, Heightmap.Types.WORLD_SURFACE_WG, level, rs)).append(',')
                    .append(floor).append(']');
            }
            sb.append(']');
        } else if (mode.equals("structures")) {
            structures(sb, seed, count);
        } else {
            throw new IllegalArgumentException("unknown mode " + mode);
        }
        try (FileWriter w = new FileWriter(out)) {
            w.write(sb.append('}').toString());
        }
    }
}
