-- DeltaMUD Database Schema
-- Based on analysis of dbinterface.c
-- This is an empty compatibility snapshot for isolated tooling and the C
-- oracle. Production Rust deployments must run `deltamud --migrate`, which
-- owns the checksummed schema_migrations ledger. No administrative identity or
-- password is seeded here.

USE deltamud;

-- Main player table with 83 columns as defined in NUM_PLAYER_MAIN_ROW_ELEMENTS
CREATE TABLE IF NOT EXISTS player_main (
    idnum INT PRIMARY KEY,
    name VARCHAR(30) NOT NULL UNIQUE,
    description TEXT,
    title VARCHAR(80),
    sex TINYINT,
    class TINYINT,
    race TINYINT,
    deity TINYINT,
    level TINYINT,
    hometown INT,
    birth BIGINT,
    played BIGINT,
    weight INT,
    height INT,
    -- PHC strings (Argon2id) exceed the legacy crypt(3) field width.
    pwd VARCHAR(255),
    last_logon BIGINT,
    host VARCHAR(80),
    
    -- Character stats
    act BIGINT,
    str TINYINT,
    str_add TINYINT,
    intel TINYINT,
    wis TINYINT,
    dex TINYINT,
    con TINYINT,
    cha TINYINT,
    
    -- Gameplay stats
    hit INT,
    max_hit INT,
    mana INT,
    max_mana INT,
    move INT,
    max_move INT,
    gold INT,
    bank_gold INT,
    exp BIGINT,
    power INT,
    mpower INT,
    defense INT,
    mdefense INT,
    technique INT,
    
    -- Player specials
    PADDING0 INT,
    talks1 INT,
    talks2 INT,
    talks3 INT,
    wimp_level INT,
    freeze_level TINYINT,
    invis_level TINYINT,
    load_room INT,
    pref BIGINT,
    bad_pws TINYINT,
    cond1 TINYINT,
    cond2 TINYINT,
    cond3 TINYINT,
    death_timer INT,
    citizen INT,
    training TINYINT,
    newbie TINYINT,
    arena INT,
    spells_to_learn INT,
    questpoints INT,
    nextquest INT,
    countdown INT,
    questobj INT,
    questmob INT,
    recall_level TINYINT,
    retreat_level TINYINT,
    trust TINYINT,
    bail_amt INT,
    wins INT,
    losses INT,
    pref2 BIGINT,
    godcmds1 BIGINT,
    godcmds2 BIGINT,
    godcmds3 BIGINT,
    godcmds4 BIGINT,
    clan INT,
    clan_rank TINYINT,
    mapx INT,
    mapy INT,
    buildmodezone INT,
    buildmoderoom INT,
    tloadroom INT,
    
    -- Character specials
    alignment INT,
    affected_by BIGINT,
    
    INDEX idx_name (name),
    INDEX idx_level (level)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Player affects table
CREATE TABLE IF NOT EXISTS player_affects (
    idnum INT NOT NULL,
    type INT,
    duration INT,
    modifier INT,
    location TINYINT,
    bitvector BIGINT,
    INDEX idx_idnum (idnum)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Player skills table
CREATE TABLE IF NOT EXISTS player_skills (
    idnum INT NOT NULL,
    skill INT,
    learned TINYINT,
    INDEX idx_idnum (idnum),
    INDEX idx_skill (skill)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
