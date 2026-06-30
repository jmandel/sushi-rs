// Pure Layer-A R4 snapshot oracle, matching the IG Publisher's model path:
// parse R4 input -> convert to R5 -> run R5 ProfileUtilities.generateSnapshot ->
// convert the result back to R4 JSON.
//
// args:
//   [--sort] [--output-r5|--output-r4] [--local-dir <dir>] [--messages <messages.json>] <cacheFolder> <pkgId#ver>... <inputProfile.json> <outputSnapshot.json>
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
    int pos = 0;
    while (pos < a.length && a[pos].startsWith("--")) {
      if ("--sort".equals(a[pos])) {
        sort = true;
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
      } else {
        throw new IllegalArgumentException("unknown option: " + a[pos]);
      }
    }
    if (a.length - pos < 4) {
      throw new IllegalArgumentException("usage: SnapOracleR4 [--sort] [--output-r5|--output-r4] [--local-dir <dir>] [--messages <messages.json>] <cacheFolder> <pkgId#ver>... <inputProfile.json> <outputSnapshot.json>");
    }

    String cache = a[pos];
    String inFile = a[a.length-2], outFile = a[a.length-1];
    FilesystemPackageCacheManager pcm = new FilesystemPackageCacheManager.Builder().withCacheFolder(cache).build();
    SimpleWorkerContext ctx = null;
    for (int i = pos + 1; i <= a.length-3; i++) {
      String[] pv = a[i].split("#");
      NpmPackage npm = pcm.loadPackageFromCacheOnly(pv[0], pv[1]);
      if (ctx == null) {
        ctx = new SimpleWorkerContext.SimpleWorkerContextBuilder().withAllowLoadingDuplicates(true).fromPackage(npm);
      } else {
        ctx.loadFromPackage(npm, null);
      }
    }
    ctx.setExpansionParameters(new org.hl7.fhir.r5.model.Parameters());
    if (localDir != null) {
      loadLocalR4CanonicalResources(ctx, localDir);
    }

    org.hl7.fhir.r4.model.StructureDefinition derivedR4 =
      (org.hl7.fhir.r4.model.StructureDefinition) new org.hl7.fhir.r4.formats.JsonParser(true, true).parse(new FileInputStream(inFile));
    StructureDefinition derived = (StructureDefinition) VersionConvertorFactory_40_50.convertResource(derivedR4);
    StructureDefinition base = ctx.fetchResource(StructureDefinition.class, derived.getBaseDefinition());
    if (base == null) { System.err.println("base not found: " + derived.getBaseDefinition()); System.exit(2); }
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
      jp.compose(new FileOutputStream(outFile), derived);
    } else {
      org.hl7.fhir.r4.formats.JsonParser jp = new org.hl7.fhir.r4.formats.JsonParser();
      jp.setOutputStyle(org.hl7.fhir.r4.formats.IParser.OutputStyle.PRETTY);
      jp.compose(new FileOutputStream(outFile), VersionConvertorFactory_40_50.convertResource(derived));
    }
    if (messagesFile != null) {
      writeMessages(messagesFile, messages, sortErrors);
    }
    System.out.println("OK r4 publisher-path output=" + (outputR5 ? "r5" : "r4") + " sort=" + sort + " snapshot.element=" + derived.getSnapshot().getElement().size() + " messages=" + messages.size() + " sortErrors=" + sortErrors.size());
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
