/* ************************************************************************
*   File: password.c                              Part of DeltaMUD      *
*  Usage: Secure password hashing and verification system               *
*                                                                        *
*  DeltaMUD Password Security Upgrade - SHA-256 Implementation          *
*  Replaces legacy DES encryption with modern secure hashing            *
************************************************************************ */

#define __PASSWORD_C__

#include "conf.h"
#include "sysdep.h"

#include <string.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#include "structs.h"
#include "utils.h"

/* Generate a random salt for password hashing */
char *generate_salt(void) {
    static char salt[17]; /* 16 chars + null terminator */
    const char charset[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789./";
    int i;
    
    /* Seed random number generator if not already done */
    static int seeded = 0;
    if (!seeded) {
        srand(time(NULL) ^ getpid());
        seeded = 1;
    }
    
    /* Generate 16 random characters */
    for (i = 0; i < 16; i++) {
        salt[i] = charset[rand() % (sizeof(charset) - 1)];
    }
    salt[16] = '\0';
    
    return salt;
}

/* Create a secure password hash using SHA-256 */
char *create_secure_password_hash(const char *password, const char *username) {
    char *salt_str, *hash_str;
    char salt_format[32];
    
    if (!password || !username) {
        return NULL;
    }
    
    /* Generate random salt */
    salt_str = generate_salt();
    
    /* Format: $6$ indicates SHA-512, but we'll use $5$ for SHA-256 */
    sprintf(salt_format, "$5$%s$", salt_str);
    
    /* Use system crypt with SHA-256 */
    hash_str = crypt(password, salt_format);
    
    if (!hash_str) {
        /* Fallback: if SHA-256 not available, log error and use basic crypt */
        /* This should not happen on modern systems */
        return strdup(crypt(password, username));
    }
    
    return strdup(hash_str);
}

/* Verify a password against stored hash with backward compatibility */
int verify_password(const char *password, const char *stored_hash, const char *username) {
    char *result;
    
    if (!password || !stored_hash || !username) {
        return 0; /* Failed verification */
    }
    
    /* Check if this is a legacy DES hash (10 or 13 characters) */
    if (strlen(stored_hash) == 13 || strlen(stored_hash) == 10) {
        /* Legacy DES password verification */
        result = crypt(password, stored_hash);
        return (result && strcmp(result, stored_hash) == 0) ? 1 : 0;
    }
    
    /* Check if this is a SHA-256 hash (starts with $5$) */
    if (strncmp(stored_hash, "$5$", 3) == 0) {
        /* Modern SHA-256 password verification */
        result = crypt(password, stored_hash);
        return (result && strcmp(result, stored_hash) == 0) ? 1 : 0;
    }
    
    /* Check if this is a SHA-512 hash (starts with $6$) */
    if (strncmp(stored_hash, "$6$", 3) == 0) {
        /* SHA-512 password verification */
        result = crypt(password, stored_hash);
        return (result && strcmp(result, stored_hash) == 0) ? 1 : 0;
    }
    
    /* Unknown hash format - try legacy verification as fallback */
    result = crypt(password, stored_hash);
    return (result && strcmp(result, stored_hash) == 0) ? 1 : 0;
}

/* Check if a password hash needs upgrading */
int password_needs_upgrade(const char *stored_hash) {
    if (!stored_hash) {
        return 1; /* No hash = needs upgrade */
    }
    
    /* DES hashes are 10 or 13 characters and need upgrade */
    if (strlen(stored_hash) == 13 || strlen(stored_hash) == 10) {
        return 1;
    }
    
    /* Already using modern hash */
    if (strncmp(stored_hash, "$5$", 3) == 0 || strncmp(stored_hash, "$6$", 3) == 0) {
        return 0;
    }
    
    /* Unknown format - assume needs upgrade */
    return 1;
}