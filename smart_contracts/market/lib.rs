#![cfg_attr(not(feature = "std"), no_std)]

use ink_lang as ink;

#[ink::contract]
mod matchingengine {
    use ink_storage::{
        collections::HashMap as HashMap,
        traits::{
            PackedLayout,
            SpreadLayout,
        },
    };

    /// Event emitted when a token is sold
    #[ink(event)]
    pub struct Sold {
        #[ink(topic)]
        seller: Option<AccountId>,
        #[ink(topic)]
        buyer: Option<AccountId>,
        #[ink(topic)]
        coll_token_id: u128,
        #[ink(topic)]
        price: Balance,
    }

    /// Withdraw types.
    #[derive(scale::Encode, scale::Decode, Clone, Copy, SpreadLayout, PackedLayout)]
    #[cfg_attr(
        feature = "std",
        derive(scale_info::TypeInfo, ink_storage::traits::StorageLayout)
    )]
    pub enum WithdrawType {
        /// Withdraw by seller after successful trade
        WithdrawMatched,
        /// Withdraw after cancelled or errored trade
        WithdrawUnused,
    }

    #[ink(storage)]
    pub struct MatchingEngine {
        /// Contract owner
        owner: AccountId,

        /// Contract admin (server that will input/output quote currency)
        admin: AccountId,

        /////////////////////////////////////////////////////////////////////////////////
        // Deposits / Balances / Withdrawals
        
        /// Balances by quote currency ID and address
        /// Quote currency ID:
        /// 1 = Unique (not live yet, so unused for now)
        /// 2 = KSM
        /// 3 = DOT
        /// 4 = EDG
        /// (...) more to come
        quote_balance: HashMap<(u64, AccountId), Balance>,

        /// Total trades counter (resettable)
        total_traded: HashMap<u64, Balance>,

        /// Vault withdraw ledger
        withdraw_queue: HashMap<u128, (AccountId, Balance, WithdrawType)>,

        /// Last withdraw id
        last_withdraw_id: u128,

        /// Recorded NFT deposits
        nft_deposits: HashMap<(u64, u64), AccountId>,

        /// NFT Vault withdraw ledger
        nft_withdraw_queue: HashMap<u128, (AccountId, u64, u64)>,

        /// Last NFT withdraw index
        last_nft_withdraw_id: u128,

        /////////////////////////////////////////////////////////////////////////////////
        // Asks

        /// Current asks: ask_id -> (collectionId, tokenId, quote_id, price, seller)
        asks: HashMap<u128, (u64, u64, u64, Balance, AccountId)>,

        /// Ask index: Helps find the ask by the colectionId + tokenId
        asks_by_token: HashMap<(u64, u64), u128>,

        /// Last Ask ID
        last_ask_id: u128,
    }

    impl MatchingEngine {

        #[ink(constructor)]
        pub fn new() -> Self {
            let mut total_traded = HashMap::new();
            total_traded.insert(2, 0);

            Self {
                owner: Self::env().caller(),
                admin: AccountId::default(),
                quote_balance: HashMap::new(),
                total_traded,
                withdraw_queue: HashMap::new(),
                last_withdraw_id: 0,
                nft_deposits: HashMap::new(),
                nft_withdraw_queue: HashMap::new(),
                last_nft_withdraw_id: 0,
                asks: HashMap::new(),
                asks_by_token: HashMap::new(),
                last_ask_id: 0,
            }

        }

        /// Returns the contract owner
        #[ink(message)]
        pub fn get_owner(&self) -> AccountId {
            self.owner.clone()
        }

        /// Set contract admin
        #[ink(message)]
        pub fn set_admin(&mut self, admin: AccountId) {
            self.ensure_only_owner();
            self.admin = admin.clone();
        }

        /// Get total
        #[ink(message)]
        pub fn get_total(&self, quote_id: u64) -> Balance {
            *self.total_traded.get(&quote_id).unwrap()
        }
        
        /// Reset total
        #[ink(message)]
        pub fn reset_total(&mut self, quote_id: u64) {
            self.ensure_only_owner();
            self.total_traded.insert(quote_id, 0);
        }

        /// Admin: Make a deposit for a user
        #[ink(message)]
        pub fn register_deposit(&mut self, quote_id: u64, deposit_balance: Balance, user: AccountId) {
            self.ensure_only_admin();

            // Check overflow
            let initial_balance = self.balance_of_or_zero(quote_id, &user);
            assert!(initial_balance + deposit_balance > initial_balance);

            // Set or update quote balance
            self.quote_balance.insert((quote_id, user.clone()), initial_balance + deposit_balance);
        }

        /// Get address balance in quote currency
        #[ink(message)]
        pub fn get_balance(&self, quote_id: u64) -> Balance {
            self.balance_of_or_zero(quote_id, &self.env().caller())
        }

        /// User: Withdraw funds
        #[ink(message)]
        pub fn withdraw(&mut self, quote_id: u64, withdraw_balance: Balance) {
            self.vault_withdraw(&self.env().caller(), quote_id, withdraw_balance, WithdrawType::WithdrawUnused);
        }

        /// Get last withdraw id
        #[ink(message)]
        pub fn get_last_withdraw_id(&self) -> u128 {
            self.last_withdraw_id
        }

        /// Get withdraw by id
        #[ink(message)]
        pub fn get_withdraw_by_id(&self, id: u128) -> (AccountId, Balance, WithdrawType) {
            *self.withdraw_queue.get(&id).unwrap()
        }

        /// Get last NFT withdraw id
        #[ink(message)]
        pub fn get_last_nft_withdraw_id(&self) -> u128 {
            self.last_nft_withdraw_id
        }

        /// Get NFT withdraw by id
        #[ink(message)]
        pub fn get_nft_withdraw_by_id(&self, id: u128) -> (AccountId, u64, u64) {
            *self.nft_withdraw_queue.get(&id).unwrap()
        }

        /// Admin: Tell the market about a successful NFT deposit
        #[ink(message)]
        pub fn register_nft_deposit(&mut self, collection_id: u64, token_id: u64, user: AccountId) {

            self.ensure_only_admin();

            // Record the token deposit for the user
            self.nft_deposits.insert((collection_id, token_id), user.clone());
        }

        /// Get deposit 
        #[ink(message)]
        pub fn get_nft_deposit(&self, collection_id: u64, token_id: u64) -> AccountId {
            *self.nft_deposits.get(&(collection_id, token_id)).unwrap()
        }

        /// User: Place a deposited NFT for sale
        #[ink(message)]
        pub fn ask(&mut self, collection_id: u64, token_id: u64, quote_id: u64, price: Balance) {

            // make sure sender owns this deposit (if not called by the admin)
            let deposit_owner = *self.nft_deposits.get(&(collection_id, token_id)).unwrap();
            if self.env().caller() != (*self).owner {
                assert_eq!(deposit_owner, self.env().caller());
            }

            // Remove a deposit
            let _ = self.nft_deposits.take(&(collection_id, token_id));

            // Place an ask (into asks with a new Ask ID)
            let ask_id = self.last_ask_id + 1;
            let ask = (collection_id, token_id, quote_id, price, deposit_owner.clone());
            self.last_ask_id = ask_id;
            self.asks.insert(ask_id, ask.clone());

            // Record that token is being sold by this user (in asks_by_token) in reverse lookup index
            self.asks_by_token.insert((collection_id, token_id), ask_id);
        }

        /// Get last ask ID
        #[ink(message)]
        pub fn get_last_ask_id(&self) -> u128 {
            self.last_ask_id
        }

        /// Get ask by ID
        #[ink(message)]
        pub fn get_ask_by_id(&self, ask_id: u128) -> (u64, u64, u64, Balance, AccountId) {
            *self.asks.get(&ask_id).unwrap()
        }

        /// Get ask by token
        #[ink(message)]
        pub fn get_ask_id_by_token(&self, collection_id: u64, token_id: u64) -> u128 {
            *self.asks_by_token.get(&(collection_id, token_id)).unwrap()
        }

        /// Cancel an ask
        #[ink(message)]
        pub fn cancel(&mut self, collection_id: u64, token_id: u64) {

            // Ensure that sender owns this ask
            let ask_id = *self.asks_by_token.get(&(collection_id, token_id)).unwrap();
            let (_, _, _, _, user) = *self.asks.get(&ask_id).unwrap();
            if self.env().caller() != self.owner {
                assert_eq!(self.env().caller(), user);
            }

            // Remove ask from everywhere
            self.remove_ask(collection_id, token_id, ask_id);

            // Transfer token back to user through NFT Vault
            self.last_nft_withdraw_id = self.last_nft_withdraw_id + 1;
            self.nft_withdraw_queue.insert(self.last_nft_withdraw_id, (user, collection_id, token_id));
        }

        /// Match an ask
        #[ink(message)]
        pub fn buy(&mut self, collection_id: u64, token_id: u64) {

            // Get the ask
            let ask_id = *self.asks_by_token.get(&(collection_id, token_id)).unwrap();
            let (_, _, quote_id, price, seller) = *self.asks.get(&ask_id).unwrap();

            // Check that buyer has enough balance
            let initial_buyer_balance = self.balance_of_or_zero(quote_id, &self.env().caller());
            assert!(initial_buyer_balance >= price);
            
            // Subtract balance from buyer and increase balance of the seller and owner (due to commission)
            let initial_seller_balance = self.balance_of_or_zero(quote_id, &seller);
            assert!(initial_seller_balance + price > initial_seller_balance); // overflow protection
            self.quote_balance.insert((quote_id, self.env().caller().clone()), initial_buyer_balance - price);
            self.quote_balance.insert((quote_id, seller.clone()), initial_seller_balance + price);

            // Remove ask from everywhere
            self.remove_ask(collection_id, token_id, ask_id);

            // Start an NFT withdraw from the vault
            self.last_nft_withdraw_id = self.last_nft_withdraw_id + 1;
            self.nft_withdraw_queue.insert(self.last_nft_withdraw_id, (self.env().caller().clone(), collection_id, token_id));

            // Start Quote withdraw from the vault for the seller
            self.vault_withdraw(&seller, quote_id, price, WithdrawType::WithdrawMatched);

            // Update totals
            let total = *self.total_traded.get(&quote_id).unwrap();
            self.total_traded.insert(quote_id, total + price);

            // Emit Sold event
            let ctid : u128 = (collection_id as u128) * 0x100000000 + (token_id as u128);
            Self::env().emit_event(Sold {
                seller: Some(seller),
                buyer: Some(self.env().caller()),
                coll_token_id: ctid,
                price: price,
            });
        }

        /// Panic if the sender is not the contract owner
        fn ensure_only_owner(&self) {
            assert_eq!(self.env().caller(), self.owner);
        }

        /// Panic if the sender is not the contract admin
        fn ensure_only_admin(&self) {
            assert_eq!(self.env().caller(), self.admin);
        }

        /// Return address balance in quote currency or 0
        fn balance_of_or_zero(&self, quote_id: u64, user: &AccountId) -> Balance {
            *self.quote_balance.get(&(quote_id, *user)).unwrap_or(&0)
        }

        fn remove_ask(&mut self, collection_id: u64, token_id: u64, ask_id: u128) {
            // Remove the record that token is being sold by this user (from asks_by_token)
            let _ = self.asks_by_token.take(&(collection_id, token_id));

            // Remove an ask (from asks)
            let _ = self.asks.take(&ask_id);
        }

        fn vault_withdraw(&mut self, user: &AccountId, quote_id: u64, withdraw_balance: Balance, withdraw_type: WithdrawType) {
            // Make sure the user has enough
            let initial_balance = self.balance_of_or_zero(quote_id, user);
            assert!(initial_balance >= withdraw_balance);

            // Update user's quote balance
            self.quote_balance.insert((quote_id, (*user).clone()), initial_balance - withdraw_balance);

            // Increase last withdraw index
            self.last_withdraw_id = self.last_withdraw_id + 1;

            // Start a withdraw from the vault
            self.withdraw_queue.insert(self.last_withdraw_id, ((*user).clone(), withdraw_balance, withdraw_type));
        }

    }
}

