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

package com.google.oak.crypto;

import com.google.oak.crypto.hpke.Context;
import com.google.oak.crypto.hpke.Hpke;
import com.google.oak.crypto.v1.AeadEncryptedMessage;
import com.google.oak.crypto.v1.EncryptedRequest;
import com.google.oak.crypto.v1.EncryptedResponse;
import com.google.oak.util.Result;
import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;
import java.util.Optional;

/**
 * Encryptor class for encrypting client requests that will be sent to the server and decrypting
 * server responses that are received by the client. Each Encryptor corresponds to a single crypto
 * session between the client and the server.
 *
 * Sequence numbers for requests and responses are incremented separately, meaning that there could
 * be multiple responses per request and multiple requests per response.
 */
public class ClientEncryptor implements Encryptor {
  // TODO(#3642): Remove this key and use a real encapsulated key generated by the client once Java
  // encryption is implemented.
  private static final byte[] TEST_ENCAPSULATED_PUBLIC_KEY = {4, 61, (byte) 141, 127, (byte) 160,
      (byte) 162, (byte) 184, (byte) 158, 72, (byte) 237, 105, 64, (byte) 182, 118, (byte) 163,
      (byte) 183, (byte) 174, 1, 81, 66, (byte) 139, 37, (byte) 218, (byte) 208, 17, (byte) 139,
      (byte) 159, (byte) 158, 68, 123, 124, 96, 114, (byte) 150, 38, (byte) 251, 112, 28, 121,
      (byte) 132, 45, (byte) 250, 118, (byte) 208, (byte) 142, (byte) 153, 124, (byte) 192,
      (byte) 139, (byte) 178, (byte) 239, (byte) 188, (byte) 177, (byte) 219, 52, (byte) 178, 123,
      117, (byte) 254, (byte) 171, (byte) 248, 77, 4, (byte) 242, 118};

  // Info string used by Hybrid Public Key Encryption.
  private static final byte[] OAK_HPKE_INFO = "Oak Hybrid Public Key Encryption v1".getBytes();

  // Encapsulated public key needed to establish a symmetric session key.
  // Only sent in the initial request message of the session.
  private Optional<byte[]> serializedEncapsulatedPublicKey;
  private final Context.SenderRequestContext senderRequestContext;
  private final Context.SenderResponseContext senderResponseContext;

  /**
   * Creates a new instance of {@code ClientEncryptor}.
   * The corresponding encryption and decryption keys are generated using the server public key with
   * Hybrid Public Key Encryption (HPKE).
   * <https://www.rfc-editor.org/rfc/rfc9180.html>.
   *
   * @param serializedServerPublicKey a NIST P-256 SEC1 encoded point public key; see
   * <https://secg.org/sec1-v2.pdf>
   */
  // TODO(#3642): Implement Java Hybrid Encryption.
  public static final Result<ClientEncryptor, Exception> create(
      final byte[] serializedServerPublicKey) {
    return Hpke.setupBaseSender(serializedServerPublicKey, OAK_HPKE_INFO).map(ClientEncryptor::new);
  }

  private ClientEncryptor(Hpke.SenderContext senderContext) {
    // TODO(#3642): Use real serialized encapsulated public key.
    this.serializedEncapsulatedPublicKey = Optional.of(TEST_ENCAPSULATED_PUBLIC_KEY);
    this.senderRequestContext = senderContext.senderRequestContext;
    this.senderResponseContext = senderContext.senderResponseContext;
  }

  /**
   * Encrypts `plaintext` and authenticates `associatedData` using AEAD.
   * <https://datatracker.ietf.org/doc/html/rfc5116>
   *
   * @param plaintext the input byte array to be encrypted
   * @param associatedData the input byte array with associated data to be authenticated
   * @return a serialized {@code EncryptedRequest} message wrapped in a {@code Result}
   */
  @Override
  public final Result<byte[], Exception> encrypt(
      final byte[] plaintext, final byte[] associatedData) {
    // Encrypt request.
    Result<byte[], Exception> sealResult =
        this.senderRequestContext.seal(plaintext, associatedData);
    if (sealResult.isError()) {
      return Result.error(sealResult.error().get());
    }
    byte[] ciphertext = sealResult.success().get();

    // Create request message.
    EncryptedRequest.Builder encryptedRequestBuilder =
        EncryptedRequest.newBuilder().setEncryptedMessage(
            AeadEncryptedMessage.newBuilder()
                .setCiphertext(ByteString.copyFrom(ciphertext))
                .setAssociatedData(ByteString.copyFrom(associatedData))
                .build());

    // Encapsulated public key is only sent in the initial request message of the session.
    if (this.serializedEncapsulatedPublicKey.isPresent()) {
      byte[] serializedEncapsulatedPublicKey = this.serializedEncapsulatedPublicKey.get();
      encryptedRequestBuilder.setSerializedEncapsulatedPublicKey(
          ByteString.copyFrom(serializedEncapsulatedPublicKey));
      this.serializedEncapsulatedPublicKey = Optional.empty();
    }
    EncryptedRequest encryptedRequest = encryptedRequestBuilder.build();

    // TODO(#3843): Return unserialized proto messages once we have Java encryption without JNI.
    return Result.success(encryptedRequest.toByteArray());
  }

  /**
   * Decrypts a {@code EncryptedResponse} proto message using AEAD.
   * <https://datatracker.ietf.org/doc/html/rfc5116>
   *
   * @param serializedEncryptedResponse a serialized {@code EncryptedResponse} message
   * @return a response message plaintext and associated data wrapped in a {@code Result}
   */
  @Override
  public final Result<Encryptor.DecryptionResult, Exception> decrypt(
      final byte[] serializedEncryptedResponse) {
    // Deserialize response message.
    EncryptedResponse encryptedResponse;
    try {
      encryptedResponse = EncryptedResponse.parseFrom(serializedEncryptedResponse);
    } catch (InvalidProtocolBufferException e) {
      return Result.error(e);
    }
    AeadEncryptedMessage aeadEncryptedMessage = encryptedResponse.getEncryptedMessage();
    byte[] ciphertext = aeadEncryptedMessage.getCiphertext().toByteArray();
    byte[] associatedData = aeadEncryptedMessage.getAssociatedData().toByteArray();

    // Decrypt response.
    return this.senderResponseContext.open(ciphertext, associatedData)
        .map(plaintext
            ->
            // TODO(#3843): Accept unserialized proto messages once we have Java encryption without
            // JNI.
            new ClientEncryptor.DecryptionResult(plaintext, associatedData));
  }
}
