// Pure Layer-A R4 snapshot oracle, matching the IG Publisher's model path:
// parse R4 input -> convert to R5 -> run R5 ProfileUtilities.generateSnapshot ->
// convert the result back to R4 JSON.
//
// args:
//   [--sort] [--output-r5|--output-r4] [--local-dir <dir>] [--messages <messages.json>] <cacheFolder> <pkgId#ver>... <inputProfile.json> <outputSnapshot.json>
//   [--sort] [--output-r5|--output-r4] [--local-dir <dir>] --batch-list <tsv> <cacheFolder> <pkgId#ver>...
//
// --dump-converted / --dump-converted-sorted (stage-2 oracle):
//   parse the R4 input SD, convert to the R5 internal model exactly as the
//   generation path does, and emit R5-internal JSON WITHOUT running
//   generateSnapshot. --dump-converted is the raw converted form (no
//   sortDifferential, no setIds). --dump-converted-sorted additionally runs
//   sortDifferential + setIds (the same prep generateSnapshot sees). Neither
//   fetches the base or runs the walk, so packages are optional in this mode.
import org.hl7.fhir.convertors.factory.VersionConvertorFactory_40_50;
import org.hl7.fhir.r5.context.SimpleWorkerContext;
import org.hl7.fhir.r5.conformance.profile.ProfileUtilities;
import org.hl7.fhir.r5.formats.IParser;
import org.hl7.fhir.r5.model.ElementDefinition;
import org.hl7.fhir.r5.model.StructureDefinition;
import org.hl7.fhir.utilities.npm.FilesystemPackageCacheManager;
import org.hl7.fhir.utilities.npm.NpmPackage;
import org.hl7.fhir.utilities.validation.ValidationMessage;
import java.io.*;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

public class SnapOracleR4 {
  public static void main(String[] a) throws Exception {
    boolean sort = false;
    boolean outputR5 = true;
    String messagesFile = null;
    String localDir = null;
    String batchList = null;
    // 0 = normal generateSnapshot; 1 = dump raw converted R5 model (no sort, no
    // generateSnapshot); 2 = dump converted R5 model after sortDifferential+setIds.
    int dumpConverted = 0;
    int pos = 0;
    while (pos < a.length && a[pos].startsWith("--")) {
      if ("--sort".equals(a[pos])) {
        sort = true;
        pos++;
      } else if ("--dump-converted".equals(a[pos])) {
        dumpConverted = 1;
        pos++;
      } else if ("--dump-converted-sorted".equals(a[pos])) {
        dumpConverted = 2;
        pos++;
      } else if ("--output-r5".equals(a[pos]) || "--native-r5".equals(a[pos])) {
        outputR5 = true;
        pos++;
      } else if ("--output-r4".equals(a[pos])) {
        outputR5 = false;
        pos++;
      } else if ("--local-dir".equals(a[pos])) {
        localDir = a[pos + 1];
        pos += 2;
      } else if ("--messages".equals(a[pos])) {
        messagesFile = a[pos + 1];
        pos += 2;
      } else if ("--batch-list".equals(a[pos])) {
        batchList = a[pos + 1];
        pos += 2;
      } else {
        throw new IllegalArgumentException("unknown option: " + a[pos]);
      }
    }
    if (batchList == null && a.length - pos < 4) {
      throw new IllegalArgumentException("usage: SnapOracleR4 [--sort] [--output-r5|--output-r4] [--local-dir <dir>] [--messages <messages.json>] <cacheFolder> <pkgId#ver>... <inputProfile.json> <outputSnapshot.json>");
    }
    if (batchList != null && a.length - pos < 1) {
      throw new IllegalArgumentException("usage: SnapOracleR4 [--sort] [--output-r5|--output-r4] [--local-dir <dir>] --batch-list <tsv> <cacheFolder> <pkgId#ver>...");
    }

    String cache = a[pos];
    String inFile = batchList == null ? a[a.length-2] : null;
    String outFile = batchList == null ? a[a.length-1] : null;
    FilesystemPackageCacheManager pcm = new FilesystemPackageCacheManager.Builder().withCacheFolder(cache).build();
    SimpleWorkerContext ctx = null;
    int packageEnd = batchList == null ? a.length - 3 : a.length;
    for (int i = pos + 1; i < packageEnd; i++) {
      String[] pv = a[i].split("#");
      NpmPackage npm = pcm.loadPackageFromCacheOnly(pv[0], pv[1]);
      if (ctx == null) {
        ctx = new SimpleWorkerContext.SimpleWorkerContextBuilder().withAllowLoadingDuplicates(true).fromPackage(npm);
      } else {
        ctx.loadFromPackage(npm, null);
      }
      loadPackageFallbackR4CanonicalResources(ctx, cache, a[i]);
    }
    // Conversion is context-free; --dump-converted needs no package context and no base.
    if (ctx != null) {
      ctx.setExpansionParameters(new org.hl7.fhir.r5.model.Parameters());
      if (localDir != null) {
        loadLocalR4CanonicalResources(ctx, localDir);
      }
    } else if (dumpConverted == 0) {
      throw new IllegalArgumentException("no packages loaded; a package context is required unless --dump-converted[-sorted]");
    }

    if (dumpConverted != 0) {
      if (batchList != null) {
        runDumpBatch(batchList, ctx, dumpConverted == 2);
      } else {
        int elements = runDumpConverted(ctx, dumpConverted == 2, inFile, outFile);
        System.out.println("OK r4 dump-converted sorted=" + (dumpConverted == 2) + " differential.element=" + elements);
      }
      return;
    }

    if (batchList != null) {
      runBatch(batchList, ctx, sort, outputR5);
      return;
    }
    RunResult result = runOne(ctx, sort, outputR5, inFile, outFile, messagesFile);
    System.out.println("OK r4 publisher-path output=" + (outputR5 ? "r5" : "r4") + " sort=" + sort + " snapshot.element=" + result.snapshotElements + " messages=" + result.messages + " sortErrors=" + result.sortErrors);
  }

  private static void runBatch(String batchList, SimpleWorkerContext ctx, boolean sort, boolean outputR5) throws Exception {
    int total = 0;
    int ok = 0;
    int failed = 0;
    try (BufferedReader reader = new BufferedReader(new FileReader(batchList))) {
      String line;
      while ((line = reader.readLine()) != null) {
        if (line.trim().isEmpty() || line.startsWith("#")) {
          continue;
        }
        total++;
        String[] parts = line.split("\t", -1);
        if (parts.length < 2) {
          failed++;
          System.err.println("FAIL oracle malformed batch line " + total + ": " + line);
          continue;
        }
        String inFile = parts[0];
        String outFile = parts[1];
        String messagesFile = parts.length > 2 && !parts[2].isEmpty() ? parts[2] : null;
        String name = new File(inFile).getName().replaceFirst("\\.json$", "");
        try {
          RunResult result = runOne(ctx, sort, outputR5, inFile, outFile, messagesFile);
          ok++;
          System.out.println("OK oracle " + name + " snapshot.element=" + result.snapshotElements + " messages=" + result.messages + " sortErrors=" + result.sortErrors);
        } catch (Exception e) {
          failed++;
          new File(outFile).delete();
          System.err.println("FAIL oracle " + name + ": " + e.getMessage());
          e.printStackTrace(System.err);
        }
      }
    }
    System.out.println("R4 ORACLE BATCH (" + (outputR5 ? "native-r5" : "r4") + "): ok=" + ok + " failed=" + failed + " total=" + total);
    if (failed != 0) {
      System.exit(1);
    }
  }

  private static RunResult runOne(SimpleWorkerContext ctx, boolean sort, boolean outputR5, String inFile, String outFile, String messagesFile) throws Exception {
    org.hl7.fhir.r4.model.StructureDefinition derivedR4 =
      (org.hl7.fhir.r4.model.StructureDefinition) new org.hl7.fhir.r4.formats.JsonParser(true, true).parse(new FileInputStream(inFile));
    StructureDefinition derived = (StructureDefinition) VersionConvertorFactory_40_50.convertResource(derivedR4);
    StructureDefinition base = ctx.fetchResource(StructureDefinition.class, derived.getBaseDefinition());
    if (base == null) { throw new IOException("base not found: " + derived.getBaseDefinition()); }
    List<ValidationMessage> messages = new ArrayList<>();
    ProfileUtilities pu = new ProfileUtilities(ctx, messages, null);
    pu.setForPublication(true);
    pu.setNewSlicingProcessing(true);
    ArrayList<String> sortErrors = new ArrayList<>();
    if (sort) {
      pu.sortDifferential(base, derived, "profile " + derived.getUrl(), sortErrors, true);
    }
    pu.setIds(derived, true);
    String p = derived.hasDifferential() && derived.getDifferential().hasElement()
      ? derived.getDifferential().getElementFirstRep().getPath()
      : null;
    if (p == null || p.contains(".")) {
      derived.getDifferential().getElement().add(0, new ElementDefinition().setPath(p == null ? derived.getType() : p.substring(0, p.indexOf("."))));
    }
    pu.generateSnapshot(base, derived, derived.getUrl(), "http://hl7.org/fhir", derived.getName());

    if (outputR5) {
      org.hl7.fhir.r5.formats.JsonParser jp = new org.hl7.fhir.r5.formats.JsonParser();
      jp.setOutputStyle(IParser.OutputStyle.PRETTY);
      new File(outFile).getParentFile().mkdirs();
      jp.compose(new FileOutputStream(outFile), derived);
    } else {
      org.hl7.fhir.r4.formats.JsonParser jp = new org.hl7.fhir.r4.formats.JsonParser();
      jp.setOutputStyle(org.hl7.fhir.r4.formats.IParser.OutputStyle.PRETTY);
      new File(outFile).getParentFile().mkdirs();
      jp.compose(new FileOutputStream(outFile), VersionConvertorFactory_40_50.convertResource(derived));
    }
    if (messagesFile != null) {
      writeMessages(messagesFile, messages, sortErrors);
    }
    return new RunResult(derived.getSnapshot().getElement().size(), messages.size(), sortErrors.size());
  }

  // Stage-2 oracle: parse R4 SD -> convert to R5 model exactly as the generation
  // path does -> emit R5-internal JSON. NO generateSnapshot. When sorted is false
  // this is the raw converted form (no sortDifferential, no setIds); when true we
  // additionally run sortDifferential + setIds (the prep generateSnapshot sees).
  private static int runDumpConverted(SimpleWorkerContext ctx, boolean sorted, String inFile, String outFile) throws Exception {
    org.hl7.fhir.r4.model.StructureDefinition derivedR4 =
      (org.hl7.fhir.r4.model.StructureDefinition) new org.hl7.fhir.r4.formats.JsonParser(true, true).parse(new FileInputStream(inFile));
    StructureDefinition derived = (StructureDefinition) VersionConvertorFactory_40_50.convertResource(derivedR4);
    if (sorted) {
      if (ctx == null) { throw new IllegalArgumentException("--dump-converted-sorted requires a package context to resolve the base"); }
      StructureDefinition base = ctx.fetchResource(StructureDefinition.class, derived.getBaseDefinition());
      if (base == null) { throw new IOException("base not found: " + derived.getBaseDefinition()); }
      List<ValidationMessage> messages = new ArrayList<>();
      ProfileUtilities pu = new ProfileUtilities(ctx, messages, null);
      pu.setForPublication(true);
      pu.setNewSlicingProcessing(true);
      ArrayList<String> sortErrors = new ArrayList<>();
      pu.sortDifferential(base, derived, "profile " + derived.getUrl(), sortErrors, true);
      pu.setIds(derived, true);
    }
    org.hl7.fhir.r5.formats.JsonParser jp = new org.hl7.fhir.r5.formats.JsonParser();
    jp.setOutputStyle(IParser.OutputStyle.PRETTY);
    new File(outFile).getParentFile().mkdirs();
    jp.compose(new FileOutputStream(outFile), derived);
    return derived.hasDifferential() ? derived.getDifferential().getElement().size() : 0;
  }

  private static void runDumpBatch(String batchList, SimpleWorkerContext ctx, boolean sorted) throws Exception {
    int total = 0;
    int ok = 0;
    int failed = 0;
    try (BufferedReader reader = new BufferedReader(new FileReader(batchList))) {
      String line;
      while ((line = reader.readLine()) != null) {
        if (line.trim().isEmpty() || line.startsWith("#")) {
          continue;
        }
        total++;
        String[] parts = line.split("\t", -1);
        if (parts.length < 2) {
          failed++;
          System.err.println("FAIL dump malformed batch line " + total + ": " + line);
          continue;
        }
        String inFile = parts[0];
        String outFile = parts[1];
        String name = new File(inFile).getName().replaceFirst("\\.json$", "");
        try {
          int elements = runDumpConverted(ctx, sorted, inFile, outFile);
          ok++;
          System.out.println("OK dump " + name + " differential.element=" + elements);
        } catch (Exception e) {
          failed++;
          new File(outFile).delete();
          System.err.println("FAIL dump " + name + ": " + e.getMessage());
          e.printStackTrace(System.err);
        }
      }
    }
    System.out.println("R4 DUMP-CONVERTED BATCH (sorted=" + sorted + "): ok=" + ok + " failed=" + failed + " total=" + total);
    if (failed != 0) {
      System.exit(1);
    }
  }

  private static class RunResult {
    final int snapshotElements;
    final int messages;
    final int sortErrors;

    RunResult(int snapshotElements, int messages, int sortErrors) {
      this.snapshotElements = snapshotElements;
      this.messages = messages;
      this.sortErrors = sortErrors;
    }
  }

  private static void loadLocalR4CanonicalResources(SimpleWorkerContext ctx, String dir) throws Exception {
    File folder = new File(dir);
    File[] files = folder.listFiles((d, name) -> name.endsWith(".json"));
    if (files == null) {
      throw new IOException("local-dir is not readable: " + dir);
    }
    Arrays.sort(files, (l, r) -> l.getName().compareTo(r.getName()));
    org.hl7.fhir.r4.formats.JsonParser parser = new org.hl7.fhir.r4.formats.JsonParser(true, true);
    for (File file : files) {
      org.hl7.fhir.r4.model.Resource r4;
      try {
        r4 = parser.parse(new FileInputStream(file));
      } catch (Exception e) {
        continue;
      }
      if (r4 instanceof org.hl7.fhir.r4.model.MetadataResource) {
        ctx.cacheResource(VersionConvertorFactory_40_50.convertResource(r4));
      }
    }
  }

  private static void loadPackageFallbackR4CanonicalResources(SimpleWorkerContext ctx, String cache, String packageSpec) throws Exception {
    File packageFolder = new File(new File(cache, packageSpec), "package");
    File index = new File(packageFolder, ".index.json");
    if (!packageFolder.isDirectory() || !index.isFile()) {
      return;
    }
    String indexJson = java.nio.file.Files.readString(index.toPath());
    if (!indexJson.matches("(?s).*\"files\"\\s*:\\s*\\[\\s*\\].*")) {
      return;
    }
    loadLocalR4CanonicalResources(ctx, packageFolder.getAbsolutePath());
  }

  private static void writeMessages(String file, List<ValidationMessage> messages, List<String> sortErrors) throws IOException {
    try (FileWriter w = new FileWriter(file)) {
      w.write("{\n  \"sortErrors\": [");
      for (int i = 0; i < sortErrors.size(); i++) {
        if (i > 0) w.write(", ");
        writeJsonString(w, sortErrors.get(i));
      }
      w.write("],\n  \"messages\": [\n");
      for (int i = 0; i < messages.size(); i++) {
        ValidationMessage m = messages.get(i);
        if (i > 0) w.write(",\n");
        w.write("    {");
        w.write("\"level\": "); writeJsonString(w, m.getLevel() == null ? null : m.getLevel().toCode());
        w.write(", \"type\": "); writeJsonString(w, m.getType() == null ? null : m.getType().toCode());
        w.write(", \"source\": "); writeJsonString(w, m.getSource() == null ? null : m.getSource().toString());
        w.write(", \"location\": "); writeJsonString(w, m.getLocation());
        w.write(", \"line\": " + m.getLine());
        w.write(", \"col\": " + m.getCol());
        w.write(", \"message\": "); writeJsonString(w, m.getMessage());
        w.write("}");
      }
      w.write("\n  ]\n}\n");
    }
  }

  private static void writeJsonString(FileWriter w, String s) throws IOException {
    if (s == null) {
      w.write("null");
      return;
    }
    w.write('"');
    for (int i = 0; i < s.length(); i++) {
      char c = s.charAt(i);
      switch (c) {
      case '"': w.write("\\\""); break;
      case '\\': w.write("\\\\"); break;
      case '\b': w.write("\\b"); break;
      case '\f': w.write("\\f"); break;
      case '\n': w.write("\\n"); break;
      case '\r': w.write("\\r"); break;
      case '\t': w.write("\\t"); break;
      default:
        if (c < 0x20) {
          w.write(String.format("\\u%04x", (int)c));
        } else {
          w.write(c);
        }
      }
    }
    w.write('"');
  }
}
