// Pure Layer-A snapshot oracle: calls ProfileUtilities.generateSnapshot directly.
// Optional --sort applies ProfileUtilities.sortDifferential first, as an explicit
// input-normalization step. There is still NO IG-Publisher canonical version pinning.
//
// args:
//   [--sort] [--messages <messages.json>] <cacheFolder> <pkgId#ver>... <inputProfile.json> <outputSnapshot.json>
import org.hl7.fhir.r5.context.SimpleWorkerContext;
import org.hl7.fhir.r5.conformance.profile.ProfileUtilities;
import org.hl7.fhir.r5.model.StructureDefinition;
import org.hl7.fhir.r5.formats.JsonParser;
import org.hl7.fhir.r5.formats.IParser;
import org.hl7.fhir.utilities.npm.FilesystemPackageCacheManager;
import org.hl7.fhir.utilities.npm.NpmPackage;
import org.hl7.fhir.utilities.validation.ValidationMessage;
import java.io.*;
import java.util.ArrayList;
import java.util.List;

public class SnapOracle {
  public static void main(String[] a) throws Exception {
    boolean sort = false;
    String messagesFile = null;
    int pos = 0;
    while (pos < a.length && a[pos].startsWith("--")) {
      if ("--sort".equals(a[pos])) {
        sort = true;
        pos++;
      } else if ("--messages".equals(a[pos])) {
        messagesFile = a[pos + 1];
        pos += 2;
      } else {
        throw new IllegalArgumentException("unknown option: " + a[pos]);
      }
    }
    if (a.length - pos < 4) {
      throw new IllegalArgumentException("usage: SnapOracle [--sort] [--messages <messages.json>] <cacheFolder> <pkgId#ver>... <inputProfile.json> <outputSnapshot.json>");
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
    StructureDefinition derived = (StructureDefinition) new JsonParser().parse(new FileInputStream(inFile));
    StructureDefinition base = ctx.fetchResource(StructureDefinition.class, derived.getBaseDefinition());
    if (base == null) { System.err.println("base not found: " + derived.getBaseDefinition()); System.exit(2); }
    List<ValidationMessage> messages = new ArrayList<>();
    ProfileUtilities pu = new ProfileUtilities(ctx, messages, null);
    ArrayList<String> sortErrors = new ArrayList<>();
    if (sort) {
      pu.sortDifferential(base, derived, derived.getName(), sortErrors, false);
    }
    pu.generateSnapshot(base, derived, derived.getUrl(), "http://hl7.org/fhir", derived.getName());
    JsonParser jp = new JsonParser(); jp.setOutputStyle(IParser.OutputStyle.PRETTY);
    jp.compose(new FileOutputStream(outFile), derived);
    if (messagesFile != null) {
      writeMessages(messagesFile, messages, sortErrors);
    }
    System.out.println("OK sort=" + sort + " snapshot.element=" + derived.getSnapshot().getElement().size() + " messages=" + messages.size() + " sortErrors=" + sortErrors.size());
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
