//
// Copyright 2023 The Project Oak Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//

package com.google.oak.transparency;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.oak.util.Result;
import java.nio.charset.StandardCharsets;
import java.util.Optional;

/**
 * Convenient struct for verifying the `signedEntryTimestamp` in a Rekor
 * LogEntry.
 *
 * This bundle can be verified using the public key from Rekor. The public key
 * can be obtained from the /api/v1/log/publicKey Rest API. For
 * {@link sigstore.dev}, it is a PEM-encoded x509/PKIX public key.
 */
public class RekorSignatureBundle {
  /**
   * Canonicalized JSON representation, based on RFC 8785 rules, of a subset of a
   * Rekor LogEntry fields that are signed to generate `signedEntryTimestamp`
   * (also a field in the Rekor LogEntry). These fields include body,
   * integratedTime, logID and logIndex.
   */
  private final String canonicalized;

  /**
   * Base64-encoded signature over the canonicalized JSON document.
   */
  private final String base64Signature;

  public RekorSignatureBundle(String canonicalized, String base64Signature) {
    this.canonicalized = canonicalized;
    this.base64Signature = base64Signature;
  }

  public String getBase64Signature() {
    return this.base64Signature;
  }

  public byte[] getCanonicalizedBytes() {
    return this.canonicalized.getBytes(StandardCharsets.UTF_8);
  }

  /**
   * Create a RekorSignatureBundle from the given LogEntry.
   *
   * @param entry
   * @return
   */
  public static Result<RekorSignatureBundle, Exception> fromRekorLogEntry(RekorLogEntry entry) {
    // Create a copy of the LogEntry, but skip the verification.
    RekorLogEntry.LogEntry entrySubset = new RekorLogEntry.LogEntry();
    entrySubset.body = entry.logEntry.body;
    entrySubset.integratedTime = entry.logEntry.integratedTime;
    entrySubset.logId = entry.logEntry.logId;
    entrySubset.logIndex = entry.logEntry.logIndex;

    // Canonicalized JSON document that is signed. Canonicalization should follow
    // the RFC 8785 rules.
    Gson gson = new GsonBuilder().create();
    String canonicalized = gson.toJson(entrySubset);

    if (entry.logEntry.verification == null) {
      return Result.error(
          new IllegalArgumentException("no verification in the log entry"));
    }

    return Result.success(
        new RekorSignatureBundle(canonicalized, entry.logEntry.verification.signedEntryTimestamp));
  }
}
