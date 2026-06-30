// Pure Layer-A snapshot oracle: calls ProfileUtilities.generateSnapshot directly
// (no IG-Publisher Layer-B passes, so NO version pinning). See snapshot-fodder/SPECIFICATION.md Part 0.
// args: <cacheFolder> <pkgId#ver>... <inputProfile.json> <outputSnapshot.json>
import org.hl7.fhir.r5.context.SimpleWorkerContext;
import org.hl7.fhir.r5.conformance.profile.ProfileUtilities;
import org.hl7.fhir.r5.model.StructureDefinition;
import org.hl7.fhir.r5.formats.JsonParser;
import org.hl7.fhir.r5.formats.IParser;
import org.hl7.fhir.utilities.npm.FilesystemPackageCacheManager;
import org.hl7.fhir.utilities.npm.NpmPackage;
import java.io.*;
import java.util.ArrayList;

public class SnapOracle {
  public static void main(String[] a) throws Exception {
    String cache = a[0];
    String inFile = a[a.length-2], outFile = a[a.length-1];
    FilesystemPackageCacheManager pcm = new FilesystemPackageCacheManager.Builder().withCacheFolder(cache).build();
    SimpleWorkerContext ctx = null;
    for (int i = 1; i <= a.length-3; i++) {
      String[] pv = a[i].split("#");
      NpmPackage npm = pcm.loadPackageFromCacheOnly(pv[0], pv[1]);
      if (ctx == null) ctx = new SimpleWorkerContext.SimpleWorkerContextBuilder().fromPackage(npm);
      else ctx.loadFromPackage(npm, null);
    }
    StructureDefinition derived = (StructureDefinition) new JsonParser().parse(new FileInputStream(inFile));
    StructureDefinition base = ctx.fetchResource(StructureDefinition.class, derived.getBaseDefinition());
    if (base == null) { System.err.println("base not found: " + derived.getBaseDefinition()); System.exit(2); }
    ProfileUtilities pu = new ProfileUtilities(ctx, new ArrayList<>(), null);
    pu.generateSnapshot(base, derived, derived.getUrl(), "http://hl7.org/fhir", derived.getName());
    JsonParser jp = new JsonParser(); jp.setOutputStyle(IParser.OutputStyle.PRETTY);
    jp.compose(new FileOutputStream(outFile), derived);
    System.out.println("OK snapshot.element=" + derived.getSnapshot().getElement().size());
  }
}
