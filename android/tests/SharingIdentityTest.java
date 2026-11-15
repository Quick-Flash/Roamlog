package dev.roamlog.android;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.Context;
import android.os.Bundle;
import java.io.File;
import java.nio.file.Files;
import java.security.KeyStore;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.Callable;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;

/** Runs only in disposable test package, never against Roamlog's identity. */
public final class SharingIdentityTest extends Instrumentation {
    private static final String ALIAS = "roamlog.sharing.identity.v1";

    @Override public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        start();
    }

    private static void check(boolean condition, String message) throws Exception {
        if (!condition) throw new Exception(message);
    }

    private static void rejects(Context context, File file, byte[] corrupt) throws Exception {
        Files.write(file.toPath(), corrupt);
        boolean rejected = false;
        try { SharingIdentity.loadOrCreate(context); }
        catch (Exception expected) { rejected = true; }
        check(rejected, "Corrupt identity accepted");
        check(Arrays.equals(corrupt, Files.readAllBytes(file.toPath())), "Corrupt identity overwritten");
    }

    @Override public void onStart() {
        Bundle result = new Bundle();
        try {
            Context context = getTargetContext();
            check(context.getPackageName().equals("dev.roamlog.sharingidentity.test"), "Wrong test package");
            File directory = new File(context.getNoBackupFilesDir(), "sharing");
            check(directory.isDirectory() || directory.mkdirs(), "Test directory unavailable");
            File file = new File(directory, "identity.v1");
            KeyStore keys = KeyStore.getInstance("AndroidKeyStore");
            keys.load(null);
            // Reset only this disposable package so instrumentation can be rerun.
            for (String name : new String[]{"identity.v1", "identity.v1.bak", "identity.v1.new"})
                Files.deleteIfExists(new File(directory, name).toPath());
            keys.deleteEntry(ALIAS);

            rejects(context, file, new byte[]{1, 2});
            check(!keys.containsAlias(ALIAS), "Corrupt file caused key creation");
            Files.delete(file.toPath());
            byte[] seed = SharingIdentity.loadOrCreate(context);
            check(seed.length == 32, "Wrong seed length");
            check(Arrays.equals(seed, SharingIdentity.loadOrCreate(context)), "Identity changed across loads");
            byte[] record = Files.readAllBytes(file.toPath());
            check(record.length == 61 && record[0] == 1, "Wrong encrypted record format");
            check(keys.getKey(ALIAS, null).getEncoded() == null, "Wrapping key exportable");

            ExecutorService workers = Executors.newFixedThreadPool(4);
            try {
                List<Callable<byte[]>> loads = new ArrayList<>();
                for (int i = 0; i < 8; i++) loads.add(() -> SharingIdentity.loadOrCreate(context));
                for (Future<byte[]> load : workers.invokeAll(loads))
                    check(Arrays.equals(seed, load.get()), "Concurrent load changed identity");
            } finally { workers.shutdownNow(); }

            byte[] tampered = record.clone();
            tampered[tampered.length - 1] ^= 1;
            rejects(context, file, tampered);
            rejects(context, file, Arrays.copyOf(record, record.length - 1));
            rejects(context, file, Arrays.copyOf(record, record.length + 1));
            byte[] version = record.clone();
            version[0] = 2;
            rejects(context, file, version);

            // AtomicFile must restore its backup instead of inventing a new identity.
            Files.write(new File(directory, "identity.v1.bak").toPath(), record);
            check(Arrays.equals(seed, SharingIdentity.loadOrCreate(context)), "Backup recovery changed identity");
            Files.delete(file.toPath());
            boolean missingFileRejected = false;
            try { SharingIdentity.loadOrCreate(context); }
            catch (Exception expected) { missingFileRejected = true; }
            check(missingFileRejected && !file.exists(), "Missing ciphertext replaced existing identity");

            Files.write(file.toPath(), record);
            keys.deleteEntry(ALIAS);
            rejects(context, file, record);

            // Explicit reset recovers from unreadable identity with a new one.
            SharingIdentity.reset(context);
            check(!file.exists() && !keys.containsAlias(ALIAS), "Reset left identity behind");
            byte[] fresh = SharingIdentity.loadOrCreate(context);
            check(!Arrays.equals(seed, fresh), "Reset reused old identity");
            check(Arrays.equals(fresh, SharingIdentity.loadOrCreate(context)), "New identity not persisted");
            result.putString("stream", "Sharing identity checks passed\n");
            finish(Activity.RESULT_OK, result);
        } catch (Exception error) {
            result.putString("stream", "Sharing identity checks failed: " + error.getClass().getSimpleName() + "\n");
            finish(Activity.RESULT_CANCELED, result);
        }
    }
}
