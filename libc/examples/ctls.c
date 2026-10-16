/* HTTPS из C через mbedTLS (фаза 65c).
 *
 * # Что она доказывает
 *
 * Что чужая библиотека TLS, собранная для FreeOS без единой правки исходников,
 * работает поверх наших сокетов BSD: рукопожатие с чужим сервером (`rustls`
 * стенда), проверка цепочки по корню из файла, имя сервера — адрес из
 * `subjectAltName`, запрос и ответ через шифрованный канал. Байты кладёт в
 * сокет сама программа (`mbedtls_ssl_set_bio`) — так же сделает libcurl.
 *
 * Проверка устроена так, чтобы её не прошёл клиент, принимающий любой
 * сертификат: сценарий спрашивает и сервер с чужим корнем, и отказ обязан
 * быть назван.
 *
 * `ctls <адрес> <порт> <корень.pem> <путь>` — GET по HTTPS; итог — строка
 * с кодом ответа и числом байт тела.
 */

#include <errno.h>
#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include <mbedtls/ctr_drbg.h>
#include <mbedtls/entropy.h>
#include <mbedtls/error.h>
#include <mbedtls/ssl.h>
#include <mbedtls/x509_crt.h>
#include <psa/crypto.h>

/* Ввод-вывод mbedTLS — прямо в сокет. */
static int send_bytes(void *context, const unsigned char *data, size_t length) {
    ssize_t sent = send(*(int *)context, data, length, 0);
    if (sent < 0) {
        return errno == EAGAIN ? MBEDTLS_ERR_SSL_WANT_WRITE : MBEDTLS_ERR_SSL_INTERNAL_ERROR;
    }
    return (int)sent;
}

static int receive_bytes(void *context, unsigned char *data, size_t length) {
    ssize_t got = recv(*(int *)context, data, length, 0);
    if (got < 0) {
        return errno == EAGAIN ? MBEDTLS_ERR_SSL_WANT_READ : MBEDTLS_ERR_SSL_INTERNAL_ERROR;
    }
    return (int)got;
}

static void fail(const char *what, int code) {
    char text[128];
    mbedtls_strerror(code, text, sizeof(text));
    printf("ctls: FAILED %s: -0x%04x %s\n", what, (unsigned)-code, text);
}

static int connect_to(const char *host, const char *port) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *target = NULL;
    if (getaddrinfo(host, port, &hints, &target) != 0) {
        printf("ctls: FAILED %s does not resolve\n", host);
        return -1;
    }
    int fd = socket(target->ai_family, target->ai_socktype, target->ai_protocol);
    if (fd < 0 || connect(fd, target->ai_addr, target->ai_addrlen) != 0) {
        printf("ctls: FAILED connect (errno %d: %s)\n", errno, strerror(errno));
        freeaddrinfo(target);
        return -1;
    }
    freeaddrinfo(target);
    return fd;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (argc != 5) {
        fprintf(stderr, "usage: ctls <host> <port> <roots.pem> <path>\n");
        return 2;
    }
    const char *host = argv[1];

    /* Ветка 3.6 считает хеши и шифры через PSA — его заводят до всего. */
    if (psa_crypto_init() != PSA_SUCCESS) {
        printf("ctls: FAILED psa_crypto_init\n");
        return 1;
    }

    mbedtls_entropy_context entropy;
    mbedtls_ctr_drbg_context random;
    mbedtls_x509_crt roots;
    mbedtls_ssl_config config;
    mbedtls_ssl_context ssl;
    mbedtls_entropy_init(&entropy);
    mbedtls_ctr_drbg_init(&random);
    mbedtls_x509_crt_init(&roots);
    mbedtls_ssl_config_init(&config);
    mbedtls_ssl_init(&ssl);

    int code = mbedtls_ctr_drbg_seed(&random, mbedtls_entropy_func, &entropy, (const unsigned char *)"ctls", 4);
    if (code != 0) {
        fail("seeding the generator", code);
        return 1;
    }
    code = mbedtls_x509_crt_parse_file(&roots, argv[3]);
    if (code != 0) {
        fail("reading the roots", code);
        return 1;
    }
    printf("ctls: roots from %s\n", argv[3]);

    mbedtls_ssl_config_defaults(&config, MBEDTLS_SSL_IS_CLIENT, MBEDTLS_SSL_TRANSPORT_STREAM,
                                MBEDTLS_SSL_PRESET_DEFAULT);
    mbedtls_ssl_conf_authmode(&config, MBEDTLS_SSL_VERIFY_REQUIRED);
    mbedtls_ssl_conf_ca_chain(&config, &roots, NULL);
    mbedtls_ssl_conf_rng(&config, mbedtls_ctr_drbg_random, &random);
    code = mbedtls_ssl_setup(&ssl, &config);
    if (code != 0) {
        fail("ssl setup", code);
        return 1;
    }
    /* Имя сверяется с сертификатом; у стенда это адрес в `subjectAltName`. */
    mbedtls_ssl_set_hostname(&ssl, host);

    int fd = connect_to(host, argv[2]);
    if (fd < 0) {
        return 1;
    }
    mbedtls_ssl_set_bio(&ssl, &fd, send_bytes, receive_bytes, NULL);

    while ((code = mbedtls_ssl_handshake(&ssl)) != 0) {
        if (code == MBEDTLS_ERR_SSL_WANT_READ || code == MBEDTLS_ERR_SSL_WANT_WRITE) {
            continue;
        }
        uint32_t flags = mbedtls_ssl_get_verify_result(&ssl);
        if (flags != 0 && flags != (uint32_t)-1) {
            char reason[256];
            mbedtls_x509_crt_verify_info(reason, sizeof(reason), "", flags);
            reason[strcspn(reason, "\n")] = '\0';
            printf("ctls: the certificate is rejected: %s\n", reason);
        } else {
            fail("handshake", code);
        }
        close(fd);
        return 1;
    }
    printf("ctls: %s with %s, the chain is verified\n", mbedtls_ssl_get_version(&ssl),
           mbedtls_ssl_get_ciphersuite(&ssl));

    char request[512];
    int length = snprintf(request, sizeof(request),
                          "GET %s HTTP/1.1\r\nHost: %s\r\nUser-Agent: ctls\r\nConnection: close\r\n\r\n", argv[4],
                          host);
    for (int written = 0; written < length;) {
        code = mbedtls_ssl_write(&ssl, (const unsigned char *)request + written, (size_t)(length - written));
        if (code == MBEDTLS_ERR_SSL_WANT_READ || code == MBEDTLS_ERR_SSL_WANT_WRITE) {
            continue;
        }
        if (code < 0) {
            fail("write", code);
            close(fd);
            return 1;
        }
        written += code;
    }

    /* Ответ — до закрытия: заголовок отдельно, тело считается. */
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    char head[1024];
    size_t head_length = 0;
    long body = -1;
    unsigned char chunk[4096];
    for (;;) {
        code = mbedtls_ssl_read(&ssl, chunk, sizeof(chunk));
        if (code == MBEDTLS_ERR_SSL_WANT_READ || code == MBEDTLS_ERR_SSL_WANT_WRITE ||
            code == MBEDTLS_ERR_SSL_RECEIVED_NEW_SESSION_TICKET) {
            continue;
        }
        if (code == MBEDTLS_ERR_SSL_PEER_CLOSE_NOTIFY || code == 0) {
            break;
        }
        if (code < 0) {
            fail("read", code);
            close(fd);
            return 1;
        }
        size_t got = (size_t)code;
        if (body >= 0) {
            body += (long)got;
            continue;
        }
        size_t room = sizeof(head) - 1 - head_length;
        size_t take = got < room ? got : room;
        memcpy(head + head_length, chunk, take);
        head_length += take;
        head[head_length] = '\0';
        char *end = strstr(head, "\r\n\r\n");
        if (end != NULL) {
            size_t header_bytes = (size_t)(end - head) + 4;
            body = (long)(head_length - header_bytes) + (long)(got - take);
        }
    }
    struct timespec finish;
    clock_gettime(CLOCK_MONOTONIC, &finish);
    long millis = (finish.tv_sec - start.tv_sec) * 1000 + (finish.tv_nsec - start.tv_nsec) / 1000000;

    head[strcspn(head, "\r\n")] = '\0';
    printf("ctls: %s\n", head);
    printf("ctls: %ld body bytes in %ld ms\n", body < 0 ? 0 : body, millis);

    mbedtls_ssl_close_notify(&ssl);
    close(fd);
    mbedtls_ssl_free(&ssl);
    mbedtls_ssl_config_free(&config);
    mbedtls_x509_crt_free(&roots);
    mbedtls_ctr_drbg_free(&random);
    mbedtls_entropy_free(&entropy);
    mbedtls_psa_crypto_free();
    return strncmp(head, "HTTP/1.1 200", 12) == 0 ? 0 : 1;
}
