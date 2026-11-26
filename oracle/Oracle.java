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
import net.minecraft.world.level.biome.BiomeManager;
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

    /// Registries as the server loads them (vanilla pack, tags bound): [0] = world, [1] = composite
    /// RegistryAccess, [2] = resource manager, [3] = layers.
    static Object[] serverRegistries() throws Exception {
        var repo = net.minecraft.server.packs.repository.ServerPacksSource.createVanillaTrustedRepository();
        repo.reload();
        repo.setSelected(java.util.List.of("vanilla"));
        var rm = new net.minecraft.server.packs.resources.MultiPackResourceManager(net.minecraft.server.packs.PackType.SERVER_DATA, repo.openAllSelected());
        var layers = net.minecraft.server.RegistryLayer.createRegistryAccess();
        var staticTags = net.minecraft.tags.TagLoader.loadTagsForExistingRegistries(rm, layers.getLayer(net.minecraft.server.RegistryLayer.STATIC));
        var ctx = net.minecraft.tags.TagLoader.buildUpdatedLookups(layers.getAccessForLoading(net.minecraft.server.RegistryLayer.WORLD), staticTags);
        var world = net.minecraft.resources.RegistryDataLoader.load(rm, ctx, net.minecraft.resources.RegistryDataLoader.WORLD_REGISTRIES, Runnable::run).join();
        staticTags.forEach(net.minecraft.core.Registry.PendingTags::apply);
        var access = layers.replaceFrom(net.minecraft.server.RegistryLayer.WORLD, world).compositeAccess();
        return new Object[]{world, access, rm, layers};
    }

    /// Chunks after the noise fill and the material rules (NoiseBasedChunkGenerator.doFill and
    /// buildSurface, called by reflection; no structures, so no beardifier; carvers not run). Chunks
    /// are picked to cover many surface biomes. Blocks: palette + run-length pairs in y, z, x order.
    static void surface(StringBuilder sb, long seed, int count) throws Exception {
        Object[] reg = serverRegistries();
        var world = (net.minecraft.core.RegistryAccess.Frozen) reg[0];
        var access = (net.minecraft.core.RegistryAccess) reg[1];
        Holder<NoiseGeneratorSettings> settings = world.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD);
        RandomState rs = RandomState.create(world.lookupOrThrow(Registries.NOISE), seed, settings.value());
        MultiNoiseBiomeSource source = MultiNoiseBiomeSource.createFromPreset(
            world.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST).getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
        NoiseBasedChunkGenerator gen = new NoiseBasedChunkGenerator(source, settings);
        LevelHeightAccessor level = LevelHeightAccessor.create(-64, 384);
        var factory = net.minecraft.world.level.chunk.PalettedContainerFactory.create(access);
        var fluidField = NoiseBasedChunkGenerator.class.getDeclaredField("globalFluidPicker");
        fluidField.setAccessible(true);
        var fluidPicker = (net.minecraft.world.level.levelgen.Aquifer.FluidPicker) ((java.util.function.Supplier<?>) fluidField.get(gen)).get();
        var doFill = NoiseBasedChunkGenerator.class.getDeclaredMethod("doFill", net.minecraft.world.level.levelgen.NoiseChunk.class, net.minecraft.world.level.chunk.ChunkAccess.class);
        doFill.setAccessible(true);
        var buildSurface = NoiseBasedChunkGenerator.class.getDeclaredMethod("buildSurface", net.minecraft.world.level.chunk.ChunkAccess.class,
            net.minecraft.world.level.levelgen.NoiseChunk.class, RandomState.class, BiomeManager.class, java.util.Set.class,
            net.minecraft.world.level.levelgen.material.rule.MaterialRule.class);
        buildSurface.setAccessible(true);
        // stored chunk biomes for the zoom, as in blockbiomes
        java.util.Map<Long, BiomeResolver> chunks = new java.util.HashMap<>();
        BiomeResolver stored = (qx, qy, qz) -> chunks.computeIfAbsent(((long) (qx >> 2) << 32) ^ ((qz >> 2) & 0xFFFFFFFFL),
            k -> source.createResolverForChunk(rs.createClimateSampler(SamplerContext.builder().enableCaches().build()), (qx >> 2) * 4, -16, (qz >> 2) * 4, 4, 96, 4))
            .getNoiseBiome(qx, Math.max(-16, Math.min(79, qy)), qz);
        BiomeManager manager = new BiomeManager(stored, BiomeManager.obfuscateSeed(seed));
        BiomeResolver point = source.createResolver(rs.createClimateSampler(SamplerContext.EMPTY_UNCACHED));
        Random rand = new Random(seed ^ 0x5EED);
        java.util.Map<String, Integer> seen = new java.util.HashMap<>();
        sb.append(",\"chunks\":[");
        int done = 0;
        for (int tries = 0; done < count && tries < 400000; tries++) {
            int range = tries % 3 == 0 ? 200_000 : 8_000;
            int cx = rand.nextInt(range) - range / 2, cz = rand.nextInt(range) - range / 2;
            // surface biome, or every few tries a cave biome (y -40 or 0)
            int qy = tries % 5 == 0 ? -10 : tries % 5 == 1 ? 0 : 16;
            String b = key(point.getNoiseBiome(cx * 4 + 2, qy, cz * 4 + 2));
            if (seen.getOrDefault(b, 0) >= 1 && tries < 390000) continue;  // one chunk per biome first
            seen.merge(b, 1, Integer::sum);
            var pos = new net.minecraft.world.level.ChunkPos(cx, cz);
            var chunk = new net.minecraft.world.level.chunk.ProtoChunk(pos, net.minecraft.world.level.chunk.UpgradeData.EMPTY, level, factory, null);
            chunk.fillBiomesFromNoise(source.createResolverForChunk(rs.createClimateSampler(SamplerContext.builder().enableCaches().build()), cx * 4, -16, cz * 4, 4, 96, 4));
            var volume = new net.minecraft.world.level.levelgen.densityfunction.DensityVolume(16, 384, 16, cx * 16, -64, cz * 16);
            try (var noise = new net.minecraft.world.level.levelgen.NoiseChunk(rs, null, settings.value(), fluidPicker,
                    net.minecraft.world.level.levelgen.blending.Blender.empty(), volume)) {
                doFill.invoke(gen, noise, chunk);
                buildSurface.invoke(gen, chunk, noise, rs, manager, source.possibleBiomes(), settings.value().materialRule().value());
            }
            if (done++ > 0) sb.append(',');
            sb.append("{\"x\":").append(cx).append(",\"z\":").append(cz).append(",\"biome\":\"").append(b).append("\",\"palette\":[");
            java.util.Map<net.minecraft.world.level.block.state.BlockState, Integer> pal = new java.util.LinkedHashMap<>();
            StringBuilder runs = new StringBuilder();
            int last = -1, run = 0;
            var mp = new net.minecraft.core.BlockPos.MutableBlockPos();
            for (int y = -64; y < 320; y++) for (int z = 0; z < 16; z++) for (int x = 0; x < 16; x++) {
                var st = chunk.getBlockState(mp.set(cx * 16 + x, y, cz * 16 + z));
                int id = pal.computeIfAbsent(st, k -> pal.size());
                if (id == last) { run++; continue; }
                if (run > 0) runs.append(runs.length() > 0 ? "," : "").append(last).append(',').append(run);
                last = id;
                run = 1;
            }
            runs.append(runs.length() > 0 ? "," : "").append(last).append(',').append(run);
            boolean f = true;
            for (var st : pal.keySet()) {
                sb.append(f ? "" : ",").append('"').append(net.minecraft.commands.arguments.blocks.BlockStateParser.serialize(st)).append('"');
                f = false;
            }
            sb.append("],\"runs\":[").append(runs).append("],\"heights\":[");
            for (int i = 0; i < 256; i++) {
                sb.append(i > 0 ? "," : "").append(chunk.getHeight(Heightmap.Types.WORLD_SURFACE_WG, i & 15, i >> 4))
                    .append(',').append(chunk.getHeight(Heightmap.Types.OCEAN_FLOOR_WG, i & 15, i >> 4));
            }
            sb.append("]}");
        }
        sb.append(']');
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
        var access = layers.replaceFrom(net.minecraft.server.RegistryLayer.WORLD, world).compositeAccess(); // static + world, as the server sees it

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

        StringBuilder piecesOut = new StringBuilder();
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
                        access, gen, source, sampler, rs, templates, seed, chunk, level, structure.biomes()::contains);
                    var stub = structure.findValidGenerationPoint(gc);
                    if (stub.isPresent()) {
                        hit = selected.structure().unwrapKey().orElseThrow().identifier().toString();
                        pos = stub.get().position();
                        // the real piece generator, continuing on the same context random
                        var pieces = stub.get().getPiecesBuilder().build().pieces();
                        var b = pieces.get(0).getBoundingBox();
                        String template = "";
                        if (pieces.get(0) instanceof net.minecraft.world.level.levelgen.structure.TemplateStructurePiece tp) {
                            var f = net.minecraft.world.level.levelgen.structure.TemplateStructurePiece.class.getDeclaredField("templateName");
                            f.setAccessible(true);
                            template = (String) f.get(tp);
                        }
                        piecesOut.append(piecesOut.length() == 0 ? "" : ",").append("[\"").append(setName).append("\",").append(cx).append(',').append(cz)
                            .append(",[").append(b.minX()).append(',').append(b.minY()).append(',').append(b.minZ()).append(',')
                            .append(b.maxX()).append(',').append(b.maxY()).append(',').append(b.maxZ()).append("],")
                            .append(pieces.size()).append(",\"").append(template).append("\"]");
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
        sb.append("],\"pieces\":[").append(piecesOut);
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
        } else if (mode.equals("blockbiomes")) {
            // BiomeManager.getBiome over what chunks store (createResolverForChunk, y clamped as in
            // ChunkAccess.getNoiseBiome), seeded like the server: obfuscateSeed(seed)
            java.util.Map<Long, BiomeResolver> chunks = new java.util.HashMap<>();
            BiomeResolver stored = (qx, qy, qz) -> chunks.computeIfAbsent(((long) (qx >> 2) << 32) ^ ((qz >> 2) & 0xFFFFFFFFL),
                k -> source.createResolverForChunk(rs.createClimateSampler(SamplerContext.builder().enableCaches().build()), (qx >> 2) * 4, -16, (qz >> 2) * 4, 4, 96, 4))
                .getNoiseBiome(qx, Math.max(-16, Math.min(79, qy)), qz);
            BiomeManager manager = new BiomeManager(stored, BiomeManager.obfuscateSeed(seed));
            sb.append(",\"points\":[");
            for (int i = 0; i < count; i++) {
                // mostly near spawn, some far out; y over the whole world and past it (clamped)
                int range = i % 8 == 0 ? 2_000_000 : 4_000;
                int x = rand.nextInt(range) - range / 2, z = rand.nextInt(range) - range / 2, y = rand.nextInt(400) - 72;
                if (i > 0) sb.append(',');
                sb.append('[').append(x).append(',').append(y).append(',').append(z)
                    .append(",\"").append(key(manager.getBiome(x, y, z))).append("\"]");
            }
            sb.append(']');
        } else if (mode.equals("spawn")) {
            // NoiseSpawnFinder (via getOrigin) for many seeds: [seed, x, z, chunkX, chunkZ]
            sb.append(",\"spawns\":[");
            for (int i = 0; i < count; i++) {
                long s = i == 0 ? seed : rand.nextLong();
                RandomState r2 = RandomState.create(lookup.lookupOrThrow(Registries.NOISE), s, settings.value());
                var pos = net.minecraft.world.level.levelgen.NoiseSpawnFinder.findSpawnPosition(settings.value().spawnTarget(),
                    r2.samplersWithContext(SamplerContext.builder().enableCaches().build()));
                var chunk = gen.getOrigin(r2);
                if (i > 0) sb.append(',');
                sb.append('[').append(s).append(',').append(pos.getX()).append(',').append(pos.getZ())
                    .append(',').append(chunk.x()).append(',').append(chunk.z()).append(']');
            }
            sb.append(']');
        } else if (mode.equals("slime")) {
            // Slime.checkSlimeSpawnRules; far chunks exercise the int overflow in x*x*4987142
            sb.append(",\"chunks\":[");
            for (int i = 0; i < count; i++) {
                int range = i % 2 == 0 ? 3_750_000 : 2_000;
                int cx = rand.nextInt(range) - range / 2, cz = rand.nextInt(range) - range / 2;
                boolean slime = net.minecraft.world.level.levelgen.WorldgenRandom.seedSlimeChunk(cx, cz, seed, 987234911L).nextInt(10) == 0;
                if (i > 0) sb.append(',');
                sb.append('[').append(cx).append(',').append(cz).append(',').append(slime).append(']');
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
        } else if (mode.equals("surface")) {
            surface(sb, seed, count);
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
