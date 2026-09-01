CREATE TABLE IF NOT EXISTS player_affects (
    idnum INT NOT NULL, type INT, duration INT, modifier INT,
    location TINYINT, bitvector BIGINT, INDEX idx_idnum (idnum)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
