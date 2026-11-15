package dev.roamlog.android;

import android.content.Context;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.AtomicFile;
import java.io.DataInputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.channels.FileChannel;
import java.nio.channels.FileLock;
import java.nio.charset.StandardCharsets;
import java.nio.file.StandardOpenOption;
import java.security.KeyStore;
import java.security.SecureRandom;
import java.util.Arrays;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/** Explicitly requested device identity only. Does not start networking or GPS. */
public final class SharingIdentity {
    private static final String ALIAS = "roamlog.sharing.identity.v1";
    private static final byte[] AAD = "roamlog/sharing/identity/1".getBytes(StandardCharsets.UTF_8);
    private static final int SEED_SIZE = 32;
    private static final int IV_SIZE = 12;
    private static final int RECORD_SIZE = 1 + IV_SIZE + SEED_SIZE + 16;

    private SharingIdentity() {}

    /** Call on worker thread. Shared by UI and recorder processes; never logs secrets. */
    public static synchronized byte[] loadOrCreate(Context context) throws Exception {
        File directory = new File(context.getNoBackupFilesDir(), "sharing");
        if (!directory.isDirectory() && !directory.mkdirs() && !directory.isDirectory())
            throw new IOException("Sharing identity directory unavailable");
        File base = new File(directory, "identity.v1");
        AtomicFile storage = new AtomicFile(base);
        try (FileChannel channel = FileChannel.open(new File(directory, "identity.lock").toPath(),
                StandardOpenOption.CREATE, StandardOpenOption.WRITE);
             FileLock lock = channel.lock()) {
            if (!lock.isValid()) throw new IOException("Sharing identity lock unavailable");
            KeyStore keys = KeyStore.getInstance("AndroidKeyStore");
            keys.load(null);
            byte[] record;
            try (FileInputStream input = storage.openRead();
                 DataInputStream data = new DataInputStream(input)) {
                record = new byte[RECORD_SIZE];
                data.readFully(record);
                if (data.read() != -1 || record[0] != 1)
                    throw new IOException("Invalid sharing identity record");
            } catch (FileNotFoundException missing) {
                // Keystore entry without ciphertext may mean data loss or interrupted creation.
                // Never replace an identity that might already be trusted by another device.
                if (base.exists() || new File(base.getPath() + ".bak").exists() || keys.containsAlias(ALIAS))
                    throw new IOException("Sharing identity unavailable; explicit reset required");
                return create(storage);
            }
            SecretKey wrapping = (SecretKey) keys.getKey(ALIAS, null);
            if (wrapping == null)
                throw new IOException("Sharing identity key unavailable; explicit reset required");
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.DECRYPT_MODE, wrapping,
                new GCMParameterSpec(128, Arrays.copyOfRange(record, 1, 1 + IV_SIZE)));
            cipher.updateAAD(AAD);
            byte[] seed = cipher.doFinal(record, 1 + IV_SIZE, RECORD_SIZE - 1 - IV_SIZE);
            if (seed.length != SEED_SIZE) {
                Arrays.fill(seed, (byte) 0);
                throw new IOException("Invalid sharing identity seed");
            }
            return seed;
        }
    }

    /** Deletes identity and its wrapping key. Friends must pair again with new identity. */
    public static synchronized void reset(Context context) throws Exception {
        File directory = new File(context.getNoBackupFilesDir(), "sharing");
        if (!directory.isDirectory() && !directory.mkdirs() && !directory.isDirectory())
            throw new IOException("Sharing identity directory unavailable");
        try (FileChannel channel = FileChannel.open(new File(directory, "identity.lock").toPath(),
                StandardOpenOption.CREATE, StandardOpenOption.WRITE);
             FileLock lock = channel.lock()) {
            if (!lock.isValid()) throw new IOException("Sharing identity lock unavailable");
            new AtomicFile(new File(directory, "identity.v1")).delete();
            KeyStore keys = KeyStore.getInstance("AndroidKeyStore");
            keys.load(null);
            if (keys.containsAlias(ALIAS)) keys.deleteEntry(ALIAS);
        }
    }

    private static byte[] create(AtomicFile storage) throws Exception {
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        generator.init(new KeyGenParameterSpec.Builder(ALIAS,
            KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
            .setKeySize(256).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build());
        SecretKey wrapping = generator.generateKey();
        byte[] seed = new byte[SEED_SIZE];
        new SecureRandom().nextBytes(seed);
        FileOutputStream output = null;
        try {
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.ENCRYPT_MODE, wrapping);
            cipher.updateAAD(AAD);
            byte[] iv = cipher.getIV();
            if (iv.length != IV_SIZE) throw new IOException("Unexpected sharing identity IV size");
            byte[] encrypted = cipher.doFinal(seed);
            byte[] record = new byte[RECORD_SIZE];
            record[0] = 1;
            System.arraycopy(iv, 0, record, 1, IV_SIZE);
            System.arraycopy(encrypted, 0, record, 1 + IV_SIZE, encrypted.length);
            output = storage.startWrite();
            output.write(record);
            output.getFD().sync();
            storage.finishWrite(output);
            output = null;
            // AtomicFile may log rename failures instead of throwing them.
            if (!Arrays.equals(record, storage.readFully()))
                throw new IOException("Sharing identity write failed");
            return seed;
        } catch (Exception error) {
            Arrays.fill(seed, (byte) 0);
            if (output != null) storage.failWrite(output);
            throw error;
        }
    }
}
