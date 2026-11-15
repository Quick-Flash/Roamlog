package dev.roamlog.android;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.provider.Settings;
import java.io.File;
import java.util.Arrays;
import org.json.JSONObject;

/** APK verification and installer handoff only; Zgui owns activity and GPS service. */
public final class Updates {
    private static long version(PackageInfo info) {
        return Build.VERSION.SDK_INT >= 28 ? info.getLongVersionCode() : info.versionCode;
    }

    @SuppressWarnings("deprecation")
    public static String call(Activity activity, String op, String path) {
        try {
            PackageManager pm = activity.getPackageManager();
            PackageInfo installed = pm.getPackageInfo(activity.getPackageName(), PackageManager.GET_SIGNATURES);
            if (op.equals("info")) {
                return new JSONObject().put("version_code", version(installed))
                    .put("version_name", installed.versionName)
                    .put("demo", activity.getPackageName().endsWith(".demo")).toString();
            }
            if (!op.equals("install")) throw new Exception("Unknown update operation");
            File apk = new File(path).getCanonicalFile();
            File shared = new File(activity.getFilesDir(), "zgui-shared").getCanonicalFile();
            if (!apk.getParentFile().equals(shared) || !apk.getName().equals("roamlog-update.apk"))
                throw new Exception("Invalid update file");
            PackageInfo incoming = pm.getPackageArchiveInfo(apk.getPath(), PackageManager.GET_SIGNATURES);
            if (incoming == null || !installed.packageName.equals(incoming.packageName))
                throw new Exception("Update package does not match this app");
            if (version(incoming) <= version(installed)) throw new Exception("Update is not newer");
            if (installed.signatures == null || incoming.signatures == null || installed.signatures.length == 0
                    || !Arrays.equals(installed.signatures, incoming.signatures))
                throw new Exception("Update signing certificate does not match; existing trips kept");
            if (Build.VERSION.SDK_INT >= 26 && !pm.canRequestPackageInstalls()) {
                activity.runOnUiThread(() -> activity.startActivity(new Intent(
                    Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:" + activity.getPackageName()))));
                return new JSONObject().put("message", "Allow updates from Roamlog, then tap Install again.").toString();
            }
            Uri uri = Uri.parse("content://" + activity.getPackageName() + ".zgui.files/" + apk.getName());
            activity.runOnUiThread(() -> activity.startActivity(new Intent(Intent.ACTION_INSTALL_PACKAGE, uri)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)));
            return new JSONObject().put("message", "Confirm update in Android installer. Trips stay on this device.").toString();
        } catch (Exception error) {
            try { return new JSONObject().put("error", error.toString()).toString(); }
            catch (Exception ignored) { return "{\"error\":\"APK installer unavailable\"}"; }
        }
    }
}
